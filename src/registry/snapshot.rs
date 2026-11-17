//! The read side of the registry: one committed state, read by the writer after
//! each commit and published whole. Queries answer from it, so no read touches
//! SQLite or waits for the writer.

use rusqlite::Connection;
use seer_sync::Cursor;
use zns_verify::Action;

use super::{core, Event, Registration};

/// One committed state of the registry.
pub(crate) struct Snapshot {
    /// Every live registration, sorted by name.
    names: Vec<Registration>,
    /// The event log, in canonical order: newest first.
    events: Vec<Event>,
    /// The sync position.
    checkpoint: Option<Cursor>,
}

/// One page of a list, and the length of the whole list.
pub(crate) type Page<T> = (Vec<T>, u64);

impl Snapshot {
    /// Reads the committed state through the registry's own queries, so the
    /// ordering, the parsed fields and the implicit releases are theirs.
    pub(crate) fn read(conn: &Connection) -> rusqlite::Result<Self> {
        let (events, _) = core::events(conn, None, None, None, u32::MAX, 0)?;
        Ok(Self {
            names: core::all_registrations(conn)?,
            events,
            checkpoint: core::checkpoint(conn)?,
        })
    }

    /// Every live registration, sorted by name.
    pub(crate) fn names(&self) -> &[Registration] {
        &self.names
    }

    /// The live registration of `name`.
    pub(crate) fn name(&self, name: &str) -> Option<&Registration> {
        self.names
            .binary_search_by(|registration| registration.name.as_str().cmp(name))
            .ok()
            .map(|index| &self.names[index])
    }

    /// A page of the live registrations, sorted by name.
    pub(crate) fn list(&self, limit: u32, offset: u32) -> Page<Registration> {
        (
            page(self.names.iter(), limit, offset),
            self.names.len() as u64,
        )
    }

    /// A page of the live registrations bound to `ua`, sorted by name.
    pub(crate) fn by_address(&self, ua: &str, limit: u32, offset: u32) -> Page<Registration> {
        let bound = || {
            self.names
                .iter()
                .filter(|registration| registration.ua == ua)
        };
        (page(bound(), limit, offset), bound().count() as u64)
    }

    /// A page of the event log, newest first. `name` and `action` match
    /// exactly; `since_height` keeps events above it.
    pub(crate) fn events(
        &self,
        name: Option<&str>,
        action: Option<Action>,
        since_height: Option<u32>,
        limit: u32,
        offset: u32,
    ) -> Page<Event> {
        let matching = || {
            self.events.iter().filter(move |event| {
                name.is_none_or(|name| event.name == name)
                    && action.is_none_or(|action| event.action == action)
                    && since_height.is_none_or(|since| event.height > since)
            })
        };
        (page(matching(), limit, offset), matching().count() as u64)
    }

    /// The sync position.
    pub(crate) fn checkpoint(&self) -> Option<Cursor> {
        self.checkpoint
    }
}

/// The items `LIMIT limit OFFSET offset` keeps.
fn page<'a, T: Clone + 'a>(items: impl Iterator<Item = &'a T>, limit: u32, offset: u32) -> Vec<T> {
    items
        .skip(offset as usize)
        .take(limit as usize)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use rusqlite::params;

    use super::*;
    use crate::registry::storage::SCHEMA_SQL;

    /// A Name Note memo's `prev` field for a first claim.
    const NO_PREV: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    /// The pages every list is compared on: `(limit, offset)`.
    const PAGES: [(u32, u32); 5] = [(1, 0), (2, 1), (10, 0), (3, 4), (5, 50)];

    /// Live names, their addresses and claim heights, and implicit releases
    /// tied at one height, tx and action.
    fn registry() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA_SQL).unwrap();
        for (index, (name, ua)) in [("c", "u"), ("a", "v"), ("e", "u"), ("b", "u"), ("d", "v")]
            .into_iter()
            .enumerate()
        {
            let height = 90 + index as u32;
            let memo = format!("ZNS:claim:{name}:{ua}:none:{NO_PREV}");
            for table in ["names", "name_events"] {
                conn.execute(
                    &format!(
                        "INSERT INTO {table} (name, height, action, ua, expires_at, prev, cmx, nullifier, txid, tx_index, action_index, memo)
                         VALUES (?1, ?2, 'claim', ?3, 'none', x'00', x'04', ?4, ?5, 0, 0, ?6)"
                    ),
                    params![name, height, ua, name.as_bytes(), vec![index as u8; 32], memo.as_bytes()],
                )
                .unwrap();
            }
        }
        for name in ["a", "b"] {
            conn.execute(
                "INSERT INTO implicit_releases (name, height, txid, tx_index, action_index, nullifier)
                 VALUES (?1, 100, ?2, 1, 2, ?3)",
                params![name, vec![9u8; 32], name.as_bytes()],
            )
            .unwrap();
        }
        conn.execute(
            "UPDATE registry_account SET sync_height = 100, sync_hash = zeroblob(32) WHERE id = 0",
            [],
        )
        .unwrap();
        conn
    }

    fn names(registrations: &[Registration]) -> Vec<&str> {
        registrations
            .iter()
            .map(|registration| registration.name.as_str())
            .collect()
    }

    fn ids(events: &[Event]) -> Vec<i64> {
        events.iter().map(|event| event.id).collect()
    }

    /// Every query the snapshot answers gives what the registry's SQL gives.
    #[test]
    fn the_snapshot_answers_as_the_registry_does() {
        let conn = registry();
        let snapshot = Snapshot::read(&conn).unwrap();

        assert_eq!(
            names(snapshot.names()),
            names(&core::all_registrations(&conn).unwrap())
        );
        assert_eq!(snapshot.checkpoint(), core::checkpoint(&conn).unwrap());
        for name in ["a", "c", "e", "missing"] {
            assert_eq!(
                snapshot
                    .name(name)
                    .map(|registration| registration.name.as_str()),
                core::resolve_by_name(&conn, name)
                    .unwrap()
                    .as_ref()
                    .map(|registration| registration.name.as_str()),
            );
        }

        for (limit, offset) in PAGES {
            let (page, total) = snapshot.list(limit, offset);
            let (expected, expected_total) =
                core::list_registrations(&conn, limit, offset).unwrap();
            assert_eq!((names(&page), total), (names(&expected), expected_total));

            for ua in ["u", "v", "w"] {
                let (page, total) = snapshot.by_address(ua, limit, offset);
                let (expected, expected_total) =
                    core::registrations_by_ua(&conn, ua, limit, offset).unwrap();
                assert_eq!((names(&page), total), (names(&expected), expected_total));
            }

            for name in [None, Some("a"), Some("c")] {
                for action in [None, Some(Action::Claim), Some(Action::Release)] {
                    for since_height in [None, Some(91), Some(100)] {
                        let (page, total) =
                            snapshot.events(name, action, since_height, limit, offset);
                        let (expected, expected_total) =
                            core::events(&conn, name, action, since_height, limit, offset).unwrap();
                        assert_eq!((ids(&page), total), (ids(&expected), expected_total));
                    }
                }
            }
        }
    }
}
