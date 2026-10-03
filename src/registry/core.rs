//! Transactional core of the registry.

use std::collections::{HashMap, HashSet};

use orchard::keys::{FullViewingKey, Scope};
use orchard::note::Note;
use orchard::note::NoteCommitTrapdoor;
use orchard::note::Nullifier;
use rusqlite::{self as rusqlite, params, Connection, OptionalExtension, Row, Transaction};
use seer_sync::{Cursor, Nullifiers, Resume};
use zcash_address::unified::Encoding as _;
use zcash_primitives::block::BlockHash;
use zcash_protocol::consensus::{BlockHeight, Parameters as _};
use zns_verify::{pallas, Action, Memo, NameNote, PrimeField, Tip};

use super::anchor_lineage::{
    keeps_anchor_note, Adoption, Lineage, Position, Retirement, TxAnchorFacts,
};
use super::batch::{BatchCandidate, BatchTx};
use super::nf::{AnchorNf, TipNf};
use super::notes;
use super::{Event, Registration};

/// The liveness interval, §4.5: a name whose tip is not renewed within
/// this many seconds of its confirmation is release-due. Mirrors
/// zns-mint's `LIVENESS_INTERVAL` (one Julian year).
///
/// TODO: move to zns-verify so the mint and the resolver share one definition.
pub(crate) const LIVENESS_INTERVAL: i64 = 31_557_600;

/// One authenticated candidate: a relaxed registry output whose memo
/// decodes to a name note that binds to its published commitment, is
/// zero-valued, and is addressed to the registry — the mint's
/// `decrypt_name_notes` gates. Parsed and verified once at triage; every
/// stage reads this form. Flattened from the scan tuple so the admission
/// boundary is constructible in tests.
struct Candidate<'a> {
    action_index: usize,
    txid: [u8; 32],
    memo: &'a [u8],
    note: NameNote<'a>,
    /// The published commitment the binding was verified against.
    cand_cmx: [u8; 32],
    /// The decrypted note: the source of the admission nullifier.
    note_orchard: Note,
    /// The verified opening of the ZNS binding.
    psi: pallas::Base,
    rcm: pallas::Scalar,
    /// Tests force the underivable-nullifier path.
    #[cfg(test)]
    fail_nullifier: bool,
}

impl Candidate<'_> {
    fn name(&self) -> &str {
        self.note.name().as_str()
    }
}

/// One transaction's law context: its identity, its pre-transaction lineage
/// snapshot, and its anchor facts. Canonical order comes from the Vec.
struct TxLaw {
    txid: [u8; 32],
    tx_index: u32,
    snapshot: Lineage,
    facts: TxAnchorFacts,
}

pub(crate) fn install_registry_config(
    conn: &Connection,
    ufvk: &str,
    network: &str,
    birthday: u32,
) -> rusqlite::Result<()> {
    let Some((stored_ufvk, stored_net, stored_birthday)) = registry_config(conn)? else {
        conn.execute(
            "INSERT INTO registry_account (id, ufvk, network, birthday) VALUES (0, ?1, ?2, ?3)",
            params![ufvk, network, birthday as i64],
        )?;
        return Ok(());
    };

    if stored_ufvk == ufvk && stored_net == network && stored_birthday == birthday as i64 {
        return Ok(());
    }

    // Nothing has been scanned, so the row is only the first-open seed and
    // can still take the configuration this process was built with.
    if !scan_has_started(conn)? {
        tracing::info!("registry has no scan yet; storing the current configuration");
        conn.execute(
            "UPDATE registry_account SET ufvk = ?1, network = ?2, birthday = ?3 WHERE id = 0",
            params![ufvk, network, birthday as i64],
        )?;
        return Ok(());
    }

    if stored_ufvk != ufvk || stored_net != network {
        return Err(config_refused(
            "registry database belongs to a different ufvk or network",
        ));
    }

    // The birthday only seeds the first sync. A database that has scanned
    // keeps the height it started from.
    tracing::warn!(
        stored = stored_birthday,
        configured = birthday,
        "stored birthday differs from this binary; keeping the stored birthday"
    );
    Ok(())
}

/// Whether any chain observation has been stored. A lone account row is not
/// a scan.
fn scan_has_started(conn: &Connection) -> rusqlite::Result<bool> {
    let started: i64 = conn.query_row(
        "SELECT (sync_height IS NOT NULL)
            OR EXISTS(SELECT 1 FROM anchor_facts)
            OR EXISTS(SELECT 1 FROM name_events)
            OR EXISTS(SELECT 1 FROM names)
            OR EXISTS(SELECT 1 FROM implicit_releases)
         FROM registry_account WHERE id = 0",
        [],
        |row| row.get(0),
    )?;
    Ok(started != 0)
}

fn config_refused(detail: &str) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
        Some(detail.to_string()),
    )
}

/// Removes payment notes from the fact table. They are not part of the fold,
/// and a database opened before that rule may still be holding them.
pub(crate) fn drop_payment_notes(conn: &Connection) -> rusqlite::Result<usize> {
    let removed = conn.execute("DELETE FROM anchor_facts WHERE value != 0", [])?;
    if removed > 0 {
        tracing::info!(removed, "dropped anchor rows that are not zero-value notes");
    }
    Ok(removed)
}

/// One triage survivor, pre-verification: everything `Candidate` needs
/// except the parse, which borrows the per-block memo arena.
struct Source<'a> {
    action_index: usize,
    cand_note: Note,
    cand_cmx: [u8; 32],
    memo: &'a [u8],
    txid: [u8; 32],
    psi: pallas::Base,
    rcm: pallas::Scalar,
}

/// The main write path: triage and anchor-fact bookkeeping, then candidate
/// admission in canonical transaction order. Admission applies the chain rule,
/// commitment binding, consumption proof, and implicit-release behavior.
/// All work runs in one transaction, so readers see pre-batch or post-batch
/// state, never partial. `lineage` is the fold of the facts at the start of
/// the batch. The connection lock makes this the sole mutator; tip reads and
/// writes share the transaction.
pub(crate) fn apply_batch(
    conn: &Connection,
    scanned: Cursor,
    transactions: &[BatchTx],
    block_times: &[(u32, u64)],
    fvk: &FullViewingKey,
    mut lineage: Lineage,
) -> rusqlite::Result<()> {
    let db_tx = conn.unchecked_transaction()?;

    // The live name nullifiers feed the claim law's no-name-spend condition.
    let mut live_names: HashSet<TipNf> = live_name_nullifiers(&db_tx)?;

    // The clock input: persist the batch's block times, then evaluate
    // each block's rules at its own MTP (median of the trailing eleven,
    // including the block itself — the mint's tracker semantics).
    for (height, time) in block_times {
        db_tx.execute(
            "INSERT OR REPLACE INTO block_times (height, time) VALUES (?1, ?2)",
            params![*height as i64, *time as i64],
        )?;
    }

    // Derivation: consider each block's candidates per name. A name note
    // references the accepted predecessor and authenticates by consuming
    // mint-owned money; the resolver's only decisions are order and state.
    for block in transactions.chunk_by(|a, b| a.height == b.height) {
        let height = u32::from(block[0].height);

        // Triage: the candidate lane. The gates are the mint's
        // `decrypt_name_notes`, so the accept-path count is mint-exact: the
        // memo decodes to a NameNote, binds to the published commitment, is
        // zero-valued, and is addressed to the registry. Candidate
        // authenticity is the recipient and binding gates — `is_sent` is
        // merged lane routing, not a signal: third-party gifts and mint
        // self-sends are both candidates. Each survivor's memo is parsed
        // and verified once, into a per-block arena the candidates borrow.
        let registry_recipient = fvk.to_ivk(Scope::External).address_at(0u32);
        let mut memos: Vec<Memo> = Vec::new();
        let mut sources: Vec<Source> = Vec::new();
        for tx in block {
            let txid = *tx.txid.as_ref();
            for BatchCandidate {
                action_index,
                note: cand_note,
                memo,
            } in &tx.relaxed_ironwood_outputs
            {
                let Some(memo) = memo else {
                    candidate_dropped("missing memo", &txid, *action_index);
                    continue;
                };
                let Ok(zns_memo) = Memo::from_bytes(memo) else {
                    candidate_dropped("undecodable memo", &txid, *action_index);
                    continue;
                };
                // Gate: protocol parse. The kernel's structural rules are the
                // authority — invalid statements never become candidates.
                let Ok(note) = NameNote::parse(&zns_memo) else {
                    candidate_dropped("memo is not a name note", &txid, *action_index);
                    continue;
                };
                // Gate: binding — the transition, hashed under the ZNS
                // binding, must reproduce the published cmx.
                let Some((psi, rcm)) =
                    notes::verify_commitment(&note, cand_note.note(), &cand_note.cmx().to_bytes())
                else {
                    candidate_dropped("commitment does not bind", &txid, *action_index);
                    continue;
                };
                // Gate: shape — zero value, registry recipient.
                if cand_note.note().value().inner() != 0 {
                    candidate_dropped("value is not zero", &txid, *action_index);
                    continue;
                }
                if cand_note.note().recipient() != registry_recipient {
                    candidate_dropped("recipient is not the registry", &txid, *action_index);
                    continue;
                }
                // Gate: the bound UA must decode as a Unified Address on this
                // network (restores 304193b, lost in the #27 refactor).
                let ua = note.ua().as_str();
                let Some((ua_network, _)) = zcash_address::unified::Address::decode(ua).ok() else {
                    candidate_dropped("ua does not decode", &txid, *action_index);
                    continue;
                };
                if ua_network != crate::NETWORK.network_type() {
                    candidate_dropped("ua is for another network", &txid, *action_index);
                    continue;
                }

                memos.push(zns_memo);
                sources.push(Source {
                    action_index: *action_index,
                    cand_note: *cand_note.note(),
                    cand_cmx: cand_note.cmx().to_bytes(),
                    memo: memo.as_slice(),
                    txid,
                    psi,
                    rcm,
                });
            }
        }
        let mut candidates = Vec::with_capacity(memos.len());
        for (memo, src) in memos.iter().zip(&sources) {
            // Triage already parsed this memo. A second failure aborts the
            // batch instead of panicking while the database lock is held.
            let note = NameNote::parse(memo)
                .map_err(|_| invariant_failure("name note failed to reparse after triage"))?;
            candidates.push(Candidate {
                action_index: src.action_index,
                txid: src.txid,
                memo: src.memo,
                note,
                cand_cmx: src.cand_cmx,
                note_orchard: src.cand_note,
                psi: src.psi,
                rcm: src.rcm,
                #[cfg(test)]
                fail_nullifier: false,
            });
        }

        // The accept-path marker per transaction: the mint offers
        // exactly-one-candidate transactions to the law; every other
        // transaction is follow_spends.
        let mut candidate_counts: HashMap<[u8; 32], usize> = HashMap::new();
        for candidate in &candidates {
            *candidate_counts.entry(candidate.txid).or_insert(0) += 1;
        }

        // Bookkeeping: the block's anchor facts land with their canonical
        // positions and candidate counts.
        for tx_data in block {
            let txid = *tx_data.txid.as_ref();
            let block_height = u32::from(tx_data.height);
            let tx_index = tx_data.tx_index;
            let candidate_count = candidate_counts.get(&txid).copied().unwrap_or(0);
            let claim_successor = claim_successor_output(tx_data, &candidates);
            let zero_value_notes = tx_data
                .ironwood_outputs
                .iter()
                .filter(|output| !output.is_sent && output.value == 0 && output.nf.is_some())
                .count();
            // The keygen transaction, or a claim's one successor. A
            // zero-value note in any other transaction does not join.
            // Spends are still recorded: a later transaction can retire
            // an anchor this one did not create.
            let keep_notes = keeps_anchor_note(zero_value_notes, candidate_count, claim_successor);
            for output in &tx_data.ironwood_outputs {
                // A payment is not an anchor fact. Storing it would grow the
                // table the lineage rereads on every batch.
                if !keep_notes || output.is_sent || output.value != 0 {
                    continue;
                }
                let Some(nf) = output.nf else {
                    continue;
                };
                let nf = AnchorNf::from_scan(nf);
                db_tx.execute(
                    "INSERT OR IGNORE INTO anchor_facts (nullifier, value, height, tx_index, action_index, name_note_candidates, claim_successor, spent_height)
                     VALUES (?1, 0, ?2, ?3, ?4, ?5, ?6, NULL)",
                    params![
                        nf.as_bytes().as_slice(),
                        block_height as i64,
                        tx_index as i64,
                        output.index as i64,
                        candidate_count as i64,
                        claim_successor as i64,
                    ],
                )?;
            }
            for spend in &tx_data.ironwood_spends {
                let nf = AnchorNf::from_scan(spend.nf);
                let updated = db_tx.execute(
                    "UPDATE anchor_facts SET spent_height = ?1, spent_tx_index = ?2, spent_action_index = ?3
                     WHERE nullifier = ?4 AND spent_height IS NULL",
                    params![
                        block_height as i64,
                        tx_index as i64,
                        spend.index as i64,
                        nf.as_bytes().as_slice()
                    ],
                )?;
                // Name-tip spends are watched and revealed here, and they are
                // not anchor facts. Any other miss is a spend we cannot place.
                if updated == 0 && !live_names.contains(&TipNf::from_revealed(&nf)) {
                    tracing::debug!(
                        nullifier = ?nf.as_bytes(),
                        height = block_height,
                        tx_index,
                        "spend matched no anchor fact"
                    );
                }
            }
        }

        // Per-transaction anchor facts and pre-transaction lineage
        // snapshots, in canonical order: a candidate is judged against the
        // lineage its own transaction was judged against.
        let mut txs: Vec<&BatchTx> = block.iter().collect();
        txs.sort_by_key(|tx| tx.tx_index);
        let mut tx_law: Vec<TxLaw> = Vec::new();
        // Names admitted earlier in this batch are not in the table yet.
        // A later claim in the same block must see them as taken.
        let mut bound_names: HashSet<String> = HashSet::new();
        for tx in txs {
            let txid = *tx.txid.as_ref();
            let tx_index = tx.tx_index;
            let name_notes = candidate_counts.get(&txid).copied().unwrap_or(0);
            let shape = claim_successor_output(tx, &candidates);
            let adoptions: Vec<Adoption> = tx
                .ironwood_outputs
                .iter()
                .filter(|o| !o.is_sent && o.value == 0)
                .filter_map(|o| o.nf.map(AnchorNf::from_scan))
                .map(|nf| Adoption { nf })
                .collect();
            let mut facts = TxAnchorFacts {
                adoptions: if keeps_anchor_note(adoptions.len(), name_notes, shape) {
                    adoptions
                } else {
                    Vec::new()
                },
                retirements: tx
                    .ironwood_spends
                    .iter()
                    .map(|s| Retirement {
                        nf: AnchorNf::from_scan(s.nf),
                    })
                    .collect(),
                has_single_name_note: name_notes == 1,
                name_notes,
                claim_successor: false,
            };
            let snapshot = lineage.clone();
            let spent_a_live_name = facts
                .retirements
                .iter()
                .any(|r| live_names.contains(&TipNf::from_revealed(&r.nf)));
            let one_live_anchor = snapshot.live_retirements(&facts) == 1;
            let admitted = if shape && one_live_anchor && !spent_a_live_name {
                admitted_claim_successor(&db_tx, &candidates, txid, &bound_names, fvk)?
            } else {
                None
            };
            if shape && admitted.is_none() {
                // The note was stored from its output shape. A rejected
                // claim does not keep it: the fold would seat it later.
                if let Some(adoption) = facts.adoptions.first() {
                    db_tx.execute(
                        "DELETE FROM anchor_facts WHERE nullifier = ?1",
                        params![adoption.nf.as_bytes().as_slice()],
                    )?;
                }
                facts.adoptions.clear();
            }
            facts.claim_successor = admitted.is_some();
            if let Some(nullifier) = admitted {
                if let Some(candidate) = candidates.iter().find(|c| c.txid == txid) {
                    bound_names.insert(candidate.name().to_string());
                }
                live_names.insert(nullifier);
            }
            lineage.step_tx(height, &facts);
            tx_law.push(TxLaw {
                txid,
                tx_index,
                snapshot,
                facts,
            });
        }

        // Consideration: canonical transaction order — the mint evaluates
        // transactions sequentially, each against the state its
        // predecessors left. The accept path admits at most one candidate
        // per transaction, so this is a total order over admissions.
        let mtp = mtp_at(&db_tx, height)?;
        apply_candidates(
            &db_tx,
            height,
            mtp,
            &candidates,
            &mut live_names,
            &tx_law,
            fvk,
        )?;
    }

    set_checkpoint_in_tx(&db_tx, &scanned)?;
    db_tx.commit()?;
    Ok(())
}

/// The zero-value anchor facts, as adoption and retirement events. The caller
/// folds them with [`fold_lineage`] after releasing the database lock.
pub(crate) fn lineage_facts(conn: &Connection) -> rusqlite::Result<Vec<(Position, FactEvent)>> {
    let mut stmt = conn.prepare(
        "SELECT nullifier, height, tx_index, action_index, name_note_candidates, claim_successor,
                spent_height, spent_tx_index, spent_action_index
         FROM anchor_facts
         WHERE value = 0",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, Option<i64>>(6)?,
            row.get::<_, Option<i64>>(7)?,
            row.get::<_, Option<i64>>(8)?,
        ))
    })?;
    let mut events = Vec::new();
    for row in rows {
        let (
            nf,
            height,
            tx_index,
            action_index,
            cands,
            claim_successor,
            spent_h,
            spent_tx,
            spent_a,
        ) = row?;
        let nf_bytes: [u8; 32] = nf.try_into().map_err(|_| corrupt_record())?;
        let nf = AnchorNf::from_bytes(&nf_bytes);
        let adopt_at = Position {
            height: row_u32(height)?,
            tx_index: row_u32(tx_index)?,
            action_index: row_u32(action_index)?,
        };
        events.push((
            adopt_at,
            FactEvent::Adoption {
                nf,
                name_note_candidates: cands,
                claim_successor: claim_successor != 0,
            },
        ));
        if let (Some(sh), Some(stx), Some(sa)) = (spent_h, spent_tx, spent_a) {
            events.push((
                Position {
                    height: row_u32(sh)?,
                    tx_index: row_u32(stx)?,
                    action_index: row_u32(sa)?,
                },
                FactEvent::Retirement(nf),
            ));
        }
    }
    Ok(events)
}

/// The mint's successor output: one registry output, zero-value and
/// received, and the transaction's one name note is a claim. A sent note
/// is not stored as an adoption, so it cannot be the successor the fold
/// seats.
/// The mint seats a successor only for a claim it admits: the name is
/// free, including names already admitted earlier in this batch, and the
/// note's nullifier derives. `None` leaves the successor out.
fn admitted_claim_successor(
    db: &Connection,
    candidates: &[Candidate<'_>],
    txid: [u8; 32],
    bound_names: &HashSet<String>,
    fvk: &FullViewingKey,
) -> rusqlite::Result<Option<TipNf>> {
    let Some(candidate) = candidates.iter().find(|candidate| candidate.txid == txid) else {
        return Ok(None);
    };
    if bound_names.contains(candidate.name()) {
        return Ok(None);
    }
    if read_tip_offline(db, candidate.name())?.is_some() {
        return Ok(None);
    }
    if notes::check_chain_rule(None, &candidate.note).is_none() {
        return Ok(None);
    }
    Ok(admit_nullifier(candidate, fvk))
}

fn claim_successor_output(tx: &BatchTx, candidates: &[Candidate<'_>]) -> bool {
    let [output] = tx.ironwood_outputs.as_slice() else {
        return false;
    };
    if output.is_sent || output.value != 0 || output.nf.is_none() {
        return false;
    }
    let txid = *tx.txid.as_ref();
    let mut notes = candidates.iter().filter(|candidate| candidate.txid == txid);
    let Some(candidate) = notes.next() else {
        return false;
    };
    notes.next().is_none() && candidate.note.action() == Action::Claim
}

fn row_u32(value: i64) -> rusqlite::Result<u32> {
    u32::try_from(value).map_err(|_| corrupt_record())
}

/// Folds anchor facts into the lineage. Each zero-value received note is an
/// adoption at its own position, each spent zero-value note a retirement at
/// its spending position. The candidate count marks the accept path, and
/// `claim_successor` marks a claim whose only registry output is that note.
pub(crate) fn fold_lineage(mut events: Vec<(Position, FactEvent)>) -> Lineage {
    events.sort_by_key(|(pos, _)| *pos);

    // Group by transaction, in canonical order.
    let mut lineage = Lineage::new();
    for group in events.chunk_by(|a, b| (a.0.height, a.0.tx_index) == (b.0.height, b.0.tx_index)) {
        let height = group[0].0.height;
        let mut facts = TxAnchorFacts::default();
        for (_, event) in group.iter().copied() {
            match event {
                FactEvent::Adoption {
                    nf,
                    name_note_candidates,
                    claim_successor,
                } => {
                    facts.has_single_name_note = name_note_candidates == 1;
                    facts.name_notes = usize::try_from(name_note_candidates).unwrap_or(usize::MAX);
                    facts.claim_successor = claim_successor;
                    facts.adoptions.push(Adoption { nf });
                }
                FactEvent::Retirement(nf) => facts.retirements.push(Retirement { nf }),
            }
        }
        lineage.step_tx(height, &facts);
    }
    // An empty lineage past the ceremony height means the scan began after
    // the keygen ceremony: every claim will be rejected (fail closed).
    tracing::debug!(
        anchors = lineage.len(),
        empty = lineage.is_empty(),
        adoption_closed = lineage.adoption_closed(),
        established = lineage.established(),
        "anchor lineage folded from facts"
    );
    lineage
}

/// One replayed fact: an adoption carrying its transaction's candidate
/// count (the accept-path marker), or a retirement.
#[derive(Debug, Clone, Copy)]
pub(crate) enum FactEvent {
    Adoption {
        nf: AnchorNf,
        name_note_candidates: i64,
        claim_successor: bool,
    },
    Retirement(AnchorNf),
}

/// The live name tips' nullifiers — the claim law's "spends no live name
/// note" set.
fn live_name_nullifiers(conn: &Connection) -> rusqlite::Result<HashSet<TipNf>> {
    let mut stmt = conn.prepare("SELECT nullifier FROM names")?;
    let rows = stmt.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
    let mut set = HashSet::new();
    for bytes in rows {
        let bytes: Vec<u8> = bytes?;
        let len = bytes.len();
        if let Ok(nf) = <[u8; 32]>::try_from(bytes) {
            set.insert(TipNf::from_bytes(&nf));
        } else {
            tracing::warn!(len, "name nullifier has an unexpected length");
        }
    }
    Ok(set)
}

/// The law, in canonical transaction order. The accept path admits at most
/// one candidate per transaction; each candidate is judged against the
/// lineage snapshot and live-name state of its own position, and every
/// admission is visible to later candidates in the same batch.
///
/// A transaction that consumes the live tip without a valid successor ends
/// that binding (the mint's `release_predecessor`), and a transaction that
/// spends any other live name tip ends that binding too (the mint's
/// `mark_released`) — both recorded as implicit releases.
fn apply_candidates(
    db_tx: &Transaction<'_>,
    height: u32,
    mtp: Option<i64>,
    candidates: &[Candidate],
    live_names: &mut HashSet<TipNf>,
    tx_law: &[TxLaw],
    fvk: &FullViewingKey,
) -> rusqlite::Result<()> {
    // The accept path admits at most one candidate per transaction, so
    // candidates index by transaction. The loop walks TRANSACTIONS in
    // canonical order — including candidate-free ones, whose follow_spends
    // behavior (ending the binding of every live name they spent) the mint
    // applies to every transaction that is not exactly-one-candidate.
    let mut candidate_by_tx: HashMap<[u8; 32], &Candidate> = HashMap::new();
    for candidate in candidates {
        candidate_by_tx.insert(candidate.txid, candidate);
    }

    for law in tx_law {
        let Some(candidate) = law
            .facts
            .has_single_name_note
            .then(|| candidate_by_tx.get(&law.txid))
            .flatten()
        else {
            // follow_spends: zero candidates, or two or more.
            mark_released_spent(
                db_tx,
                live_names,
                &law.facts,
                height,
                &law.txid,
                law.tx_index,
                0,
            )?;
            continue;
        };
        let snapshot = &law.snapshot;
        let tx_facts = &law.facts;

        let binding = read_tip_offline(db_tx, candidate.name())?;
        let note = &candidate.note;
        // The predecessor spend and the name-note output are different
        // actions. The mint's `predecessor_spent` looks at every nullifier
        // in the transaction, not at the output's own action.
        let tip_nf = binding.as_ref().map(|b| b.nullifier);
        let tip_consumed = tip_nf.is_some_and(|nf| {
            tx_facts
                .retirements
                .iter()
                .any(|r| TipNf::from_revealed(&r.nf) == nf)
        });
        let spent_a_live_name = tx_facts
            .retirements
            .iter()
            .any(|r| live_names.contains(&TipNf::from_revealed(&r.nf)));

        match note.action() {
            Action::Claim => {
                // Gate: the mint's accept_claim. The anchor spend and the
                // name note are separate actions — fee inputs sit between
                // them — so the name note's own nullifier is not the anchor.
                // The transaction must spend exactly one live anchor, spend
                // no live name, and its registry outputs must be the one
                // zero-value successor of a claim.
                let law_ok = snapshot.live_retirements(tx_facts) == 1
                    && !spent_a_live_name
                    && tx_facts.claim_successor;
                if !law_ok {
                    // The mint's mark_released: every live name this
                    // transaction spent ends its binding.
                    mark_released_spent(
                        db_tx,
                        live_names,
                        tx_facts,
                        height,
                        &candidate.txid,
                        law.tx_index,
                        candidate.action_index,
                    )?;
                    continue;
                }

                // Gate: chain rule — a claim's predecessor is zero, which
                // also requires the name free.
                let Some(expected_prev) =
                    notes::check_chain_rule(binding.as_ref().map(|b| &b.tip), note)
                else {
                    admission_dropped("chain rule", candidate.name(), &candidate.txid, height);
                    continue;
                };

                let Some(nullifier) = admit_nullifier(candidate, fvk) else {
                    admission_dropped(
                        "nullifier underivable",
                        candidate.name(),
                        &candidate.txid,
                        height,
                    );
                    continue;
                };
                if record_admission(
                    db_tx,
                    &AdmissionRow {
                        name: candidate.name(),
                        height: height as i64,
                        action: note.action().as_str(),
                        ua: note.ua().as_str(),
                        expires_at: &note
                            .expires_at()
                            .map(|e| e.field_bytes().to_string())
                            .unwrap_or_else(|| "none".to_string()),
                        prev_rcm: expected_prev.as_slice(),
                        rcm: candidate.rcm.to_repr().as_slice(),
                        psi: candidate.psi.to_repr().as_slice(),
                        cmx: &candidate.cand_cmx,
                        nullifier: nullifier.as_bytes(),
                        txid: candidate.txid.as_slice(),
                        tx_index: law.tx_index as i64,
                        action_index: candidate.action_index as i64,
                        confirmed_mtp: mtp,
                        memo: candidate.memo,
                    },
                )? {
                    live_names.insert(nullifier);
                }
            }
            Action::Update | Action::Release => {
                // Gate: predecessor — the transaction must consume this
                // name's live tip, only this name's, and no anchor. Anything
                // else is follow_spends: every spent live name ends its
                // binding.
                let spent_other_live = tx_facts.retirements.iter().any(|r| {
                    let nf = TipNf::from_revealed(&r.nf);
                    live_names.contains(&nf) && tip_nf != Some(nf)
                });
                if !tip_consumed || spent_other_live || snapshot.touches_live_anchor(tx_facts) {
                    mark_released_spent(
                        db_tx,
                        live_names,
                        tx_facts,
                        height,
                        &candidate.txid,
                        law.tx_index,
                        candidate.action_index,
                    )?;
                    continue;
                }

                // The tip was consumed: a valid successor advances an update
                // or lands a proper release; an invalid one ends the binding
                // — the mint's release_predecessor.
                let Some(expected_prev) =
                    notes::check_chain_rule(binding.as_ref().map(|b| &b.tip), note)
                else {
                    admission_dropped("chain rule", candidate.name(), &candidate.txid, height);
                    if let Some(consumed) = tip_nf {
                        end_binding_implicitly(
                            db_tx,
                            live_names,
                            candidate.name(),
                            height,
                            &candidate.txid,
                            law.tx_index,
                            candidate.action_index,
                            &consumed,
                        )?;
                    }
                    continue;
                };

                // The mint's clock law, §4.5: an update confirming after the
                // predecessor's term or liveness deadline is a release, not
                // a renewal. The mint frees the name (`release_predecessor`).
                // Releases stay legal after either clock.
                if note.action() == Action::Update {
                    if let (Some(mtp), Some(live)) = (mtp, binding.as_ref()) {
                        let liveness_due = live
                            .confirmed_mtp
                            .is_some_and(|confirmed| mtp >= confirmed + LIVENESS_INTERVAL);
                        if term_expired(&live.expires_at, mtp) || liveness_due {
                            end_binding_implicitly(
                                db_tx,
                                live_names,
                                candidate.name(),
                                height,
                                &candidate.txid,
                                law.tx_index,
                                candidate.action_index,
                                &live.nullifier,
                            )?;
                            continue;
                        }
                    }
                }

                let Some(nullifier) = admit_nullifier(candidate, fvk) else {
                    // The tip was spent on chain. Leaving the names row in
                    // place would keep serving a note that can never be spent
                    // again.
                    admission_dropped(
                        "nullifier underivable",
                        candidate.name(),
                        &candidate.txid,
                        height,
                    );
                    if let Some(consumed) = tip_nf {
                        end_binding_implicitly(
                            db_tx,
                            live_names,
                            candidate.name(),
                            height,
                            &candidate.txid,
                            law.tx_index,
                            candidate.action_index,
                            &consumed,
                        )?;
                    }
                    continue;
                };
                if record_admission(
                    db_tx,
                    &AdmissionRow {
                        name: candidate.name(),
                        height: height as i64,
                        action: note.action().as_str(),
                        ua: note.ua().as_str(),
                        expires_at: &note
                            .expires_at()
                            .map(|e| e.field_bytes().to_string())
                            .unwrap_or_else(|| "none".to_string()),
                        prev_rcm: expected_prev.as_slice(),
                        rcm: candidate.rcm.to_repr().as_slice(),
                        psi: candidate.psi.to_repr().as_slice(),
                        cmx: &candidate.cand_cmx,
                        nullifier: nullifier.as_bytes(),
                        txid: candidate.txid.as_slice(),
                        tx_index: law.tx_index as i64,
                        action_index: candidate.action_index as i64,
                        confirmed_mtp: mtp,
                        memo: candidate.memo,
                    },
                )? {
                    if let Some(live) = binding.as_ref() {
                        live_names.remove(&live.nullifier);
                    }
                    if note.action() == Action::Update {
                        live_names.insert(nullifier);
                    }
                }
            }
        }
    }
    Ok(())
}

/// The MTP at `height`: the median of the trailing eleven block times,
/// the block itself included — the mint tracker's semantics. `None`
/// until the window is complete (the scan's first blocks).
fn mtp_at(conn: &Connection, height: u32) -> rusqlite::Result<Option<i64>> {
    let start = height.saturating_sub(10);
    let mut stmt =
        conn.prepare("SELECT time FROM block_times WHERE height BETWEEN ?1 AND ?2 ORDER BY time")?;
    let times: Vec<i64> = stmt
        .query_map(params![start as i64, height as i64], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok((times.len() == 11).then(|| times[5]))
}

/// The predecessor's term at `mtp`. `"none"` does not lapse. A field that
/// is not a timestamp is already past.
fn term_expired(expires_at: &str, mtp: i64) -> bool {
    match expires_at {
        "none" => false,
        field => field
            .parse::<i64>()
            .map(|seconds| seconds <= mtp)
            .unwrap_or(true),
    }
}

fn candidate_dropped(reason: &'static str, txid: &[u8; 32], action_index: usize) {
    tracing::debug!(reason, txid = ?txid, action_index, "candidate dropped");
}

fn admission_dropped(reason: &'static str, name: &str, txid: &[u8; 32], height: u32) {
    tracing::warn!(reason, name, txid = ?txid, height, "admission dropped");
}

/// The admission nullifier of a verified candidate.
fn admit_nullifier(candidate: &Candidate, fvk: &FullViewingKey) -> Option<TipNf> {
    #[cfg(test)]
    if candidate.fail_nullifier {
        return None;
    }
    candidate
        .note_orchard
        .zns_nullifier(
            fvk,
            NoteCommitTrapdoor::from_inner(candidate.rcm),
            candidate.psi,
        )
        .map(TipNf::from_scan)
}

/// The mint's `mark_released`: every live name whose tip this transaction
/// spent ends its binding, recorded as an implicit release.
fn mark_released_spent(
    db_tx: &Transaction<'_>,
    live_names: &mut HashSet<TipNf>,
    facts: &TxAnchorFacts,
    height: u32,
    txid: &[u8],
    tx_index: u32,
    action_index: usize,
) -> rusqlite::Result<()> {
    for retirement in &facts.retirements {
        let nf = TipNf::from_revealed(&retirement.nf);
        if !live_names.contains(&nf) {
            continue;
        }
        let name: String = db_tx
            .query_row(
                "SELECT name FROM names WHERE nullifier = ?1",
                params![nf.as_bytes()],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| invariant_failure("live-name set does not match the names table"))?;
        live_names.remove(&nf);
        db_tx.execute("DELETE FROM names WHERE name = ?1", params![name])?;
        db_tx.execute(
            "INSERT OR IGNORE INTO implicit_releases (name, height, txid, tx_index, action_index, nullifier)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![name, height as i64, txid, tx_index as i64, action_index as i64, nf.as_bytes()],
        )?;
    }
    Ok(())
}

/// The mint's `release_predecessor`: the tip was consumed without a valid
/// successor, so the binding ends — recorded as an implicit release.
#[allow(clippy::too_many_arguments)]
fn end_binding_implicitly(
    db_tx: &Transaction<'_>,
    live_names: &mut HashSet<TipNf>,
    name: &str,
    height: u32,
    txid: &[u8],
    tx_index: u32,
    action_index: usize,
    consumed: &TipNf,
) -> rusqlite::Result<()> {
    live_names.remove(consumed);
    db_tx.execute("DELETE FROM names WHERE name = ?1", params![name])?;
    db_tx.execute(
        "INSERT OR IGNORE INTO implicit_releases (name, height, txid, tx_index, action_index, nullifier)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            name,
            height as i64,
            txid,
            tx_index as i64,
            action_index as i64,
            consumed.as_bytes()
        ],
    )?;
    Ok(())
}

/// The columns of one admitted candidate, written to the event log and the
/// per-name tip in one call.
struct AdmissionRow<'a> {
    name: &'a str,
    height: i64,
    action: &'a str,
    ua: &'a str,
    expires_at: &'a str,
    prev_rcm: &'a [u8],
    rcm: &'a [u8],
    psi: &'a [u8],
    cmx: &'a [u8],
    nullifier: &'a [u8],
    txid: &'a [u8],
    tx_index: i64,
    action_index: i64,
    confirmed_mtp: Option<i64>,
    memo: &'a [u8],
}

/// Writes the event, then the per-name tip. Returns whether this call
/// inserted the event. A replay of the same primary key leaves the tip
/// where it stands.
fn record_admission(db_tx: &Transaction<'_>, row: &AdmissionRow<'_>) -> rusqlite::Result<bool> {
    let sql_params = params![
        row.name,
        row.height,
        row.action,
        row.ua,
        row.expires_at,
        row.prev_rcm,
        row.rcm,
        row.psi,
        row.cmx,
        row.nullifier,
        row.txid,
        row.tx_index,
        row.action_index,
        row.confirmed_mtp,
        row.memo,
    ];
    let inserted = db_tx.execute(
        "INSERT OR IGNORE INTO name_events (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, confirmed_mtp, memo)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        sql_params,
    )?;
    if inserted == 0 {
        return Ok(false);
    }
    if row.action == "release" {
        db_tx.execute("DELETE FROM names WHERE name = ?1", params![row.name])?;
    } else {
        db_tx.execute(
            "INSERT INTO names (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, confirmed_mtp, memo)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
             ON CONFLICT (name) DO UPDATE SET
               height = excluded.height, action = excluded.action, ua = excluded.ua,
               expires_at = excluded.expires_at,
               prev_rcm = excluded.prev_rcm, rcm = excluded.rcm, psi = excluded.psi,
               cmx = excluded.cmx, nullifier = excluded.nullifier,
               txid = excluded.txid, tx_index = excluded.tx_index,
               action_index = excluded.action_index,
               confirmed_mtp = excluded.confirmed_mtp,
               memo = excluded.memo",
            sql_params,
        )?;
    }
    Ok(true)
}

pub(crate) fn rewind(conn: &Connection, fork_height: u32) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    // seer-sync re-fetches the block before `fork_height`. That block stays;
    // record_admission ignores the replay. Rows at `fork_height` and above
    // are the discarded chain.
    let fork = i64::from(fork_height);

    let mut stmt = tx.prepare(
        "SELECT name FROM name_events WHERE height >= ?1
         UNION
         SELECT name FROM implicit_releases WHERE height >= ?1",
    )?;
    let affected: Vec<String> = stmt
        .query_map(params![fork], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    tx.execute("DELETE FROM name_events WHERE height >= ?1", params![fork])?;
    tx.execute(
        "DELETE FROM implicit_releases WHERE height >= ?1",
        params![fork],
    )?;
    tx.execute("DELETE FROM block_times WHERE height >= ?1", params![fork])?;
    tx.execute("DELETE FROM anchor_facts WHERE height >= ?1", params![fork])?;
    tx.execute(
        "UPDATE anchor_facts
         SET spent_height = NULL, spent_tx_index = NULL, spent_action_index = NULL
         WHERE spent_height >= ?1",
        params![fork],
    )?;

    for name in &affected {
        rebuild_name_tip(&tx, name)?;
    }

    // The sync position falls back to the fork height; the hash is fixed by
    // the next apply (seer-sync's rewind convention).
    tx.execute(
        "UPDATE registry_account SET sync_height = ?1, sync_hash = NULL WHERE id = 0",
        params![fork_height as i64],
    )?;

    tx.commit()?;
    Ok(())
}

// ── reads (free functions; called directly on a locked connection) ──────

pub(crate) fn resume(conn: &Connection) -> rusqlite::Result<Resume> {
    let checkpoint = checkpoint(conn)?;
    let ironwood: Vec<Nullifier> = ironwood_nullifiers(conn)?
        .into_iter()
        .filter_map(|bytes| match Option::from(Nullifier::from_bytes(&bytes)) {
            Some(decoded) => Some(decoded),
            None => {
                tracing::warn!(nullifier = ?bytes, "watch-set nullifier failed to decode");
                None
            }
        })
        .collect();
    let birthday = birthday(conn)?;

    Ok(Resume {
        birthday: BlockHeight::from_u32(birthday),
        checkpoint,
        nullifiers: Nullifiers {
            sapling: vec![],
            orchard: vec![],
            ironwood,
        },
    })
}

fn birthday(conn: &Connection) -> rusqlite::Result<u32> {
    let b: i64 = conn.query_row(
        "SELECT birthday FROM registry_account WHERE id = 0",
        [],
        |row| row.get(0),
    )?;
    Ok(b as u32)
}

/// The sync position, decoded from the registry's account row. `None` = no
/// checkpoint yet (or one that failed to decode — healed by rescanning from
/// the birthday; the index replays idempotently).
pub(crate) fn checkpoint(conn: &Connection) -> rusqlite::Result<Option<Cursor>> {
    let row: Option<(Option<i64>, Option<Vec<u8>>)> = conn
        .query_row(
            "SELECT sync_height, sync_hash FROM registry_account WHERE id = 0",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((height, hash)) = row else {
        return Ok(None);
    };
    match (height, hash) {
        (Some(h), Some(bytes)) if bytes.len() == 32 => Ok(Some(Cursor {
            height: BlockHeight::from_u32(h as u32),
            hash: BlockHash(bytes.as_slice().try_into().expect("32 bytes checked above")),
        })),
        (Some(_), Some(bytes)) => {
            tracing::warn!(
                bytes = bytes.len(),
                "sync position is corrupt; rescanning from the birthday"
            );
            Ok(None)
        }
        _ => Ok(None),
    }
}

/// The watch-set: every nullifier whose consumption we must detect —
/// unspent anchors plus every admitted name tip. The two families meet
/// here as bytes for seer-sync, which has one ironwood watch list.
pub(crate) fn ironwood_nullifiers(conn: &Connection) -> rusqlite::Result<Vec<[u8; 32]>> {
    let mut statement = conn.prepare(
        "SELECT nullifier FROM anchor_facts
         WHERE value = 0 AND spent_height IS NULL
         UNION
         SELECT nullifier FROM names",
    )?;
    let rows = statement.query_map([], |row| {
        let bytes: Vec<u8> = row.get(0)?;
        bytes.try_into().map_err(|_| corrupt_record())
    })?;
    rows.collect()
}

/// The registry's UFVK. The account row is guaranteed at open; a missing row
/// is a broken database and fails loudly.
pub(crate) fn registry_ufvk(conn: &Connection) -> rusqlite::Result<String> {
    conn.query_row(
        "SELECT ufvk FROM registry_account WHERE id = 0",
        [],
        |row| row.get(0),
    )
}

pub(crate) fn name_count(conn: &Connection) -> rusqlite::Result<u64> {
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM names", [], |r| r.get(0))?;
    Ok(n as u64)
}

pub(crate) fn resolve_by_name(
    conn: &Connection,
    name: &str,
) -> rusqlite::Result<Option<Registration>> {
    conn.query_row(
        "SELECT name, ua, txid, height, action, memo FROM names WHERE name = ?1",
        params![name],
        registration_from_row,
    )
    .optional()
}

pub(crate) fn registrations_by_ua(
    conn: &Connection,
    ua: &str,
    limit: u32,
    offset: u32,
) -> rusqlite::Result<(Vec<Registration>, u64)> {
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM names WHERE ua = ?1",
        params![ua],
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare(
        "SELECT name, ua, txid, height, action, memo FROM names
         WHERE ua = ?1 ORDER BY name LIMIT ?2 OFFSET ?3",
    )?;
    let rows = stmt
        .query_map(params![ua, limit, offset], registration_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((rows, total as u64))
}

pub(crate) fn list_registrations(
    conn: &Connection,
    limit: u32,
    offset: u32,
) -> rusqlite::Result<(Vec<Registration>, u64)> {
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM names", [], |r| r.get(0))?;
    let mut stmt = conn.prepare(
        "SELECT name, ua, txid, height, action, memo FROM names ORDER BY name LIMIT ?1 OFFSET ?2",
    )?;
    let rows = stmt
        .query_map(params![limit, offset], registration_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((rows, total as u64))
}

pub(crate) fn events(
    conn: &Connection,
    name: Option<&str>,
    action: Option<Action>,
    since_height: Option<u32>,
    limit: u32,
    offset: u32,
) -> rusqlite::Result<(Vec<Event>, u64)> {
    // An implicit release has no memo of its own. It is served as a release
    // of the binding it ended, and its id is the negation of its rowid so it
    // cannot collide with a name_events rowid.
    const LOG: &str = "
        SELECT id, name, action, ua, txid, height, action_index, memo, tx_index
        FROM (
            SELECT rowid AS id, name, action, ua, txid, height, action_index, memo, tx_index
            FROM name_events
            UNION ALL
            SELECT -ir.rowid, ir.name, 'release', pred.ua, ir.txid, ir.height,
                   ir.action_index, pred.memo, ir.tx_index
            FROM implicit_releases AS ir
            JOIN name_events AS pred ON pred.rowid = (
                SELECT rowid FROM name_events
                WHERE name = ir.name
                  AND (height, tx_index, action_index)
                      < (ir.height, ir.tx_index, ir.action_index)
                ORDER BY height DESC, tx_index DESC, action_index DESC
                LIMIT 1
            )
        )";
    const WHERE: &str = "WHERE (?1 IS NULL OR name = ?1)
                         AND (?2 IS NULL OR action = ?2)
                         AND (?3 IS NULL OR height > ?3)";
    let p = params![
        name,
        action.map(|a| a.as_str()),
        since_height.map(|h| h as i64),
        limit,
        offset
    ];

    let unmatched: i64 = conn.query_row(
        "SELECT COUNT(*) FROM implicit_releases AS ir
         WHERE NOT EXISTS (
             SELECT 1 FROM name_events
             WHERE name = ir.name
               AND (height, tx_index, action_index) < (ir.height, ir.tx_index, ir.action_index)
         )",
        [],
        |row| row.get(0),
    )?;
    if unmatched != 0 {
        return Err(corrupt_record());
    }

    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM ({LOG}) {WHERE}"),
        &p[..3],
        |r| r.get(0),
    )?;
    // One spend can end several names at the same height, tx, and action.
    // `id` is unique, so a page cannot skip or repeat a tied row.
    let mut stmt = conn.prepare(&format!(
        "SELECT id, name, action, ua, txid, height, action_index, memo FROM ({LOG}) {WHERE}
         ORDER BY height DESC, tx_index DESC, action_index DESC, id DESC LIMIT ?4 OFFSET ?5"
    ))?;
    let events = stmt
        .query_map(p, |row| {
            let id: i64 = row.get(0)?;
            let name: String = row.get(1)?;
            let action: String = row.get(2)?;
            let ua: String = row.get(3)?;
            let txid: Vec<u8> = row.get(4)?;
            let height: i64 = row.get(5)?;
            let action_index: i64 = row.get(6)?;
            let memo: Vec<u8> = row.get(7)?;
            let zns_memo = Memo::from_bytes(&memo).map_err(|_| corrupt_record())?;
            let note = parse_stored_record(&zns_memo, &name, &ua).ok_or(corrupt_record())?;
            let txid: [u8; 32] = txid.try_into().map_err(|_| corrupt_record())?;
            Ok(Event {
                id,
                name: note.name().as_str().to_string(),
                action: Action::from_bytes(action.as_bytes()).ok_or(corrupt_record())?,
                ua: note.ua().as_str().to_string(),
                txid,
                height: height as u32,
                action_index: action_index as usize,
                expires_at: note
                    .expires_at()
                    .map(|e| e.field_bytes().to_string())
                    .unwrap_or_else(|| "none".to_string()),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((events, total as u64))
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn registry_config(conn: &Connection) -> rusqlite::Result<Option<(String, String, i64)>> {
    conn.query_row(
        "SELECT ufvk, network, birthday FROM registry_account WHERE id = 0",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .optional()
}

/// A name's live tip, read inside the batch transaction so a candidate sees
/// every earlier admission in the same batch.
struct LiveTip {
    tip: Tip,
    nullifier: TipNf,
    confirmed_mtp: Option<i64>,
    expires_at: String,
}

fn read_tip_offline(conn: &Connection, name: &str) -> rusqlite::Result<Option<LiveTip>> {
    conn.query_row(
        "SELECT action, rcm, nullifier, confirmed_mtp, expires_at FROM names WHERE name = ?1",
        params![name],
        |row| {
            let action =
                Action::from_bytes(row.get::<_, String>(0)?.as_bytes()).ok_or(corrupt_record())?;
            let rcm: Vec<u8> = row.get(1)?;
            let rcm: [u8; 32] = rcm.try_into().map_err(|_| corrupt_record())?;
            let nullifier: Vec<u8> = row.get(2)?;
            let nullifier: [u8; 32] = nullifier.try_into().map_err(|_| corrupt_record())?;
            Ok(LiveTip {
                tip: Tip { action, rcm },
                nullifier: TipNf::from_bytes(&nullifier),
                confirmed_mtp: row.get(3)?,
                expires_at: row.get(4)?,
            })
        },
    )
    .optional()
}

fn set_checkpoint_in_tx(tx: &Transaction<'_>, scanned: &Cursor) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE registry_account SET sync_height = ?1, sync_hash = ?2 WHERE id = 0",
        params![u32::from(scanned.height) as i64, scanned.hash.0.as_slice()],
    )?;
    Ok(())
}

/// After deleting post-fork events, set `names` to the highest surviving event
/// for this name (or delete the row if the tip was a release). The surviving
/// memo must still parse and agree with its columns — a corrupt record fails
/// the rewind loudly.
fn rebuild_name_tip(tx: &Transaction<'_>, name: &str) -> rusqlite::Result<()> {
    // The tip is the latest record across BOTH streams — explicit events and
    // implicit releases — in canonical order. An implicit release ends the
    // binding just as a proper release event does.
    let latest_implicit: Option<(i64, i64, i64)> = tx
        .query_row(
            "SELECT height, tx_index, action_index FROM implicit_releases WHERE name = ?1
             ORDER BY height DESC, tx_index DESC, action_index DESC LIMIT 1",
            params![name],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let row = tx
        .query_row(
            "SELECT action, memo, txid, height, tx_index, action_index, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, confirmed_mtp
             FROM name_events WHERE name = ?1
             ORDER BY height DESC, tx_index DESC, action_index DESC LIMIT 1",
            params![name],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Vec<u8>>(8)?,
                    row.get::<_, Vec<u8>>(9)?,
                    row.get::<_, Vec<u8>>(10)?,
                    row.get::<_, Vec<u8>>(11)?,
                    row.get::<_, Vec<u8>>(12)?,
                    row.get::<_, Option<i64>>(13)?,
                ))
            },
        )
        .optional()?;

    tx.execute("DELETE FROM names WHERE name = ?1", params![name])?;
    let Some((
        action_col,
        memo,
        txid_b,
        height,
        tx_index,
        action_index,
        ua_b,
        expires_col,
        prev_b,
        rcm_b,
        psi_b,
        cmx_b,
        nullifier_b,
        confirmed_mtp,
    )) = row
    else {
        return Ok(());
    };
    if let Some(implicit) = latest_implicit {
        // An implicit release at or after the last event ends the binding.
        if implicit >= (height, tx_index, action_index) {
            return Ok(());
        }
    }

    // The restored record must still parse and agree with its columns.
    let zns_memo = Memo::from_bytes(&memo).map_err(|_| corrupt_record())?;
    if parse_stored_record(&zns_memo, name, &ua_b).is_none() {
        return Err(corrupt_record());
    }
    let action = Action::from_bytes(action_col.as_bytes()).ok_or(corrupt_record())?;

    if matches!(action, Action::Claim | Action::Update) {
        tx.execute(
            "INSERT INTO names (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, confirmed_mtp, memo)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                name,
                height,
                action_col,
                ua_b,
                expires_col,
                prev_b,
                rcm_b,
                psi_b,
                cmx_b,
                nullifier_b,
                txid_b,
                tx_index,
                action_index,
                confirmed_mtp,
                memo,
            ],
        )?;
    }
    Ok(())
}

/// The error for a record that fails the read-time check: its memo no longer
/// parses, or disagrees with its identity columns. Corrupt and must not be
/// served.
fn corrupt_record() -> rusqlite::Error {
    invariant_failure("registry record is corrupt")
}

/// A state the scan must not commit: the batch returns this and rolls back
/// instead of panicking under the database lock.
fn invariant_failure(detail: &str) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
        Some(detail.to_string()),
    )
}

/// Parses a stored memo and cross-checks it against the record's identity
/// columns. A record whose memo no longer parses or disagrees with its
/// columns is corrupt and must not be served.
fn parse_stored_record<'a>(
    zns_memo: &'a zns_verify::Memo,
    name: &str,
    ua: &str,
) -> Option<zns_verify::NameNote<'a>> {
    let note = NameNote::parse(zns_memo).ok()?;
    if note.name().as_str() != name || note.ua().as_str() != ua {
        return None;
    }
    Some(note)
}

/// Builds a `Registration` from a `names` record: the memo is parsed and
/// cross-checked against the identity columns before anything is served.
fn registration_from_row(r: &Row<'_>) -> rusqlite::Result<Registration> {
    let name: String = r.get(0)?;
    let ua: String = r.get(1)?;
    let txid: Vec<u8> = r.get(2)?;
    let height: i64 = r.get(3)?;
    let action: String = r.get(4)?;
    let memo: Vec<u8> = r.get(5)?;

    let zns_memo = Memo::from_bytes(&memo).map_err(|_| corrupt_record())?;
    let Some(note) = parse_stored_record(&zns_memo, &name, &ua) else {
        return Err(corrupt_record());
    };
    let txid: [u8; 32] = txid.try_into().map_err(|_| corrupt_record())?;
    Ok(Registration {
        name: note.name().as_str().to_string(),
        ua: note.ua().as_str().to_string(),
        expires_at: note
            .expires_at()
            .map(|e| e.field_bytes().to_string())
            .unwrap_or_else(|| "none".to_string()),
        txid,
        height: height as u32,
        last_action: Action::from_bytes(action.as_bytes()).ok_or(corrupt_record())?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::storage::SCHEMA_SQL;

    fn database() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA_SQL).unwrap();
        conn
    }

    /// A rewound implicit release restores the binding it ended: the name
    /// returns to its last surviving event's state.
    #[test]
    fn rewound_implicit_release_restores_the_binding() {
        let conn = database();
        conn.execute(
            "INSERT INTO registry_account (id, ufvk, network, birthday) VALUES (0, 'ufvk', 'test', 1)",
            [],
        )
        .unwrap();
        insert_checkpoint(&conn, 42, 7);
        // z: claim event at 90; implicit release at 100.
        let memo =
            b"ZNS:claim:z:u:none:0000000000000000000000000000000000000000000000000000000000000000";
        conn.execute(
            "INSERT INTO name_events (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, confirmed_mtp, memo)
             VALUES ('z', 90, 'claim', 'u', 'none', x'00', x'02', x'03', x'04', x'5a', x'06', 0, 0, 0, ?1)",
            params![&memo[..]],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO names (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, confirmed_mtp, memo)
             VALUES ('z', 90, 'claim', 'u', 'none', x'00', x'02', x'03', x'04', x'5a', x'06', 0, 0, 0, ?1)",
            params![&memo[..]],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO implicit_releases (name, height, txid, tx_index, action_index, nullifier)
             VALUES ('z', 100, x'06', 0, 0, x'5a')",
            [],
        )
        .unwrap();
        // The implicit release had deleted the binding.
        conn.execute("DELETE FROM names WHERE name = 'z'", [])
            .unwrap();

        rewind(&conn, 95).unwrap();

        let tip: Option<String> = conn
            .query_row("SELECT action FROM names WHERE name = 'z'", [], |r| {
                r.get(0)
            })
            .optional()
            .unwrap();
        assert_eq!(tip.as_deref(), Some("claim"));
        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM implicit_releases", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 0);
    }

    /// A surviving implicit release keeps the binding ended: a re-claim
    /// above the fork is rewound away, and the rebuild does not resurrect
    /// the pre-release event.
    #[test]
    fn surviving_implicit_release_keeps_the_binding_ended() {
        let conn = database();
        conn.execute(
            "INSERT INTO registry_account (id, ufvk, network, birthday) VALUES (0, 'ufvk', 'test', 1)",
            [],
        )
        .unwrap();
        insert_checkpoint(&conn, 42, 7);
        let memo =
            b"ZNS:claim:z:u:none:0000000000000000000000000000000000000000000000000000000000000000";
        conn.execute(
            "INSERT INTO name_events (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, confirmed_mtp, memo)
             VALUES ('z', 90, 'claim', 'u', 'none', x'00', x'02', x'03', x'04', x'5a', x'06', 0, 0, 0, ?1)",
            params![&memo[..]],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO implicit_releases (name, height, txid, tx_index, action_index, nullifier)
             VALUES ('z', 95, x'06', 0, 0, x'5a')",
            [],
        )
        .unwrap();
        // A re-claim above the fork: rewinding past it must not resurrect
        // the pre-release event.
        conn.execute(
            "INSERT INTO name_events (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, confirmed_mtp, memo)
             VALUES ('z', 100, 'claim', 'u', 'none', x'00', x'02', x'03', x'04', x'5b', x'08', 0, 0, 0, x'09')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO names (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, confirmed_mtp, memo)
             VALUES ('z', 100, 'claim', 'u', 'none', x'00', x'02', x'03', x'04', x'5b', x'08', 0, 0, 0, x'09')",
            [],
        )
        .unwrap();

        rewind(&conn, 97).unwrap();

        let z: i64 = conn
            .query_row("SELECT COUNT(*) FROM names WHERE name = 'z'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(z, 0, "the surviving implicit release keeps z ended");
        let events: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM name_events WHERE name = 'z'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(events, 1, "only the pre-release event survives");
    }

    /// A pre-lineage database (user_version 0: old watch table, no fact
    /// tables, stale checkpoint) is wiped on open, so the next scan replays
    /// from the birthday instead of resuming past history it cannot interpret.
    #[test]
    fn pre_lineage_database_is_wiped_clean() {
        // A raw connection: the old-shape database predates the schema
        // installer entirely (user_version 0, legacy watch table, stale
        // checkpoint that would otherwise skip the ceremony).
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE watched_ironwood_notes (
                nullifier BLOB NOT NULL PRIMARY KEY,
                txid BLOB NOT NULL,
                height INTEGER NOT NULL,
                spent_height INTEGER
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE registry_account (
                id INTEGER NOT NULL PRIMARY KEY CHECK (id = 0),
                ufvk TEXT NOT NULL,
                network TEXT NOT NULL,
                birthday INTEGER NOT NULL,
                sync_height INTEGER,
                sync_hash BLOB
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO registry_account (id, ufvk, network, birthday) VALUES (0, 'ufvk', 'test', 1)",
            [],
        )
        .unwrap();
        insert_checkpoint(&conn, 42, 7);

        crate::registry::storage::install_schema(&conn).unwrap();

        let watched = conn.query_row("SELECT COUNT(*) FROM watched_ironwood_notes", [], |r| {
            r.get::<_, i64>(0)
        });
        assert!(
            watched.is_err(),
            "the legacy table is dropped, not merely emptied"
        );
        let position: Option<i64> = conn
            .query_row("SELECT sync_height FROM registry_account", [], |r| r.get(0))
            .optional()
            .unwrap();
        assert!(position.is_none(), "the stale checkpoint is wiped");
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 5);
        let facts: i64 = conn
            .query_row("SELECT COUNT(*) FROM anchor_facts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(facts, 0);
    }

    /// The current schema survives a second open. The gate used to be
    /// `version < 2` while the schema wrote version 1, so every startup
    /// wiped the database.
    #[test]
    fn current_schema_survives_reinstall() {
        let conn = database();
        conn.execute(
            "INSERT INTO registry_account (id, ufvk, network, birthday) VALUES (0, 'ufvk', 'test', 1)",
            [],
        )
        .unwrap();
        insert_checkpoint(&conn, 42, 7);
        conn.execute(
            "INSERT INTO anchor_facts (nullifier, value, height, tx_index, action_index, name_note_candidates, spent_height)
             VALUES (?1, 0, 10, 0, 0, 0, NULL)",
            params![vec![1u8; 32]],
        )
        .unwrap();

        crate::registry::storage::install_schema(&conn).unwrap();

        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 5);
        let facts: i64 = conn
            .query_row("SELECT COUNT(*) FROM anchor_facts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(facts, 1, "anchor facts survive a second install");
        let position: Option<i64> = conn
            .query_row("SELECT sync_height FROM registry_account", [], |r| r.get(0))
            .optional()
            .unwrap();
        assert_eq!(
            position,
            Some(42),
            "the checkpoint survives a second install"
        );
    }

    /// A version 1 database has tips and no confirmation times. Opening it
    /// drops the scan, keeps the account, and leaves an empty index so the
    /// next run replays from the birthday.
    #[test]
    fn version_1_database_rescans_from_the_birthday() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "PRAGMA user_version = 1;
             CREATE TABLE registry_account (
                id INTEGER NOT NULL PRIMARY KEY CHECK (id = 0),
                ufvk TEXT NOT NULL,
                network TEXT NOT NULL,
                birthday INTEGER NOT NULL,
                sync_height INTEGER,
                sync_hash BLOB
             );
             CREATE TABLE names (
                name TEXT NOT NULL PRIMARY KEY,
                height INTEGER NOT NULL,
                action TEXT NOT NULL,
                ua TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                prev_rcm BLOB NOT NULL,
                rcm BLOB NOT NULL,
                psi BLOB NOT NULL,
                cmx BLOB NOT NULL,
                nullifier BLOB NOT NULL,
                txid BLOB NOT NULL,
                tx_index INTEGER NOT NULL,
                action_index INTEGER NOT NULL,
                memo BLOB NOT NULL
             );
             CREATE TABLE anchor_facts (
                nullifier BLOB NOT NULL PRIMARY KEY,
                value INTEGER NOT NULL,
                height INTEGER NOT NULL,
                tx_index INTEGER NOT NULL,
                action_index INTEGER NOT NULL,
                name_note_candidates INTEGER NOT NULL
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO registry_account (id, ufvk, network, birthday, sync_height, sync_hash)
             VALUES (0, 'ufvk', 'test', 90, 100, ?1)",
            params![vec![1u8; 32]],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO names (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, memo)
             VALUES ('z', 1, 'claim', 'u', 'none', x'00', x'00', x'00', x'00', x'00', x'00', 0, 0, x'00')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO anchor_facts (nullifier, value, height, tx_index, action_index, name_note_candidates)
             VALUES (x'01', 0, 90, 0, 0, 0)",
            [],
        )
        .unwrap();

        crate::registry::storage::install_schema(&conn).unwrap();

        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 5);
        let birthday: i64 = conn
            .query_row(
                "SELECT birthday FROM registry_account WHERE id = 0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(birthday, 90);
        assert!(checkpoint(&conn).unwrap().is_none());
        for table in ["names", "anchor_facts", "block_times", "name_events"] {
            let rows: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(rows, 0, "{table}");
        }
    }

    /// A version 2 database cannot tell a claim successor from any other
    /// zero-value note. Opening it drops the scan and keeps the account.
    #[test]
    fn version_2_database_rescans_for_the_successor_shape() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "PRAGMA user_version = 2;
             CREATE TABLE registry_account (
                id INTEGER NOT NULL PRIMARY KEY CHECK (id = 0),
                ufvk TEXT NOT NULL,
                network TEXT NOT NULL,
                birthday INTEGER NOT NULL,
                sync_height INTEGER,
                sync_hash BLOB
             );
             CREATE TABLE anchor_facts (
                nullifier BLOB NOT NULL PRIMARY KEY,
                value INTEGER NOT NULL,
                height INTEGER NOT NULL,
                tx_index INTEGER NOT NULL,
                action_index INTEGER NOT NULL,
                name_note_candidates INTEGER NOT NULL,
                spent_height INTEGER
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO registry_account (id, ufvk, network, birthday, sync_height, sync_hash)
             VALUES (0, 'ufvk', 'test', 90, 100, ?1)",
            params![vec![1u8; 32]],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO anchor_facts (nullifier, value, height, tx_index, action_index, name_note_candidates, spent_height)
             VALUES (x'01', 0, 90, 0, 0, 1, NULL)",
            [],
        )
        .unwrap();

        crate::registry::storage::install_schema(&conn).unwrap();

        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 5);
        let birthday: i64 = conn
            .query_row(
                "SELECT birthday FROM registry_account WHERE id = 0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(birthday, 90);
        assert!(checkpoint(&conn).unwrap().is_none());
        let facts: i64 = conn
            .query_row("SELECT COUNT(*) FROM anchor_facts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(facts, 0);
    }

    /// A version 3 database stored every zero-value note. Opening it drops
    /// the scan and keeps the account, so the next run replays the keygen
    /// transaction from the birthday.
    #[test]
    fn version_3_database_rescans_for_the_keygen_transaction() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "PRAGMA user_version = 3;
             CREATE TABLE registry_account (
                id INTEGER NOT NULL PRIMARY KEY CHECK (id = 0),
                ufvk TEXT NOT NULL,
                network TEXT NOT NULL,
                birthday INTEGER NOT NULL,
                sync_height INTEGER,
                sync_hash BLOB
             );
             CREATE TABLE names (
                name TEXT NOT NULL PRIMARY KEY,
                height INTEGER NOT NULL,
                action TEXT NOT NULL,
                ua TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                prev_rcm BLOB NOT NULL,
                rcm BLOB NOT NULL,
                psi BLOB NOT NULL,
                cmx BLOB NOT NULL,
                nullifier BLOB NOT NULL,
                txid BLOB NOT NULL,
                tx_index INTEGER NOT NULL,
                action_index INTEGER NOT NULL,
                memo BLOB NOT NULL
             );
             CREATE TABLE anchor_facts (
                nullifier BLOB NOT NULL PRIMARY KEY,
                value INTEGER NOT NULL,
                height INTEGER NOT NULL,
                tx_index INTEGER NOT NULL,
                action_index INTEGER NOT NULL,
                name_note_candidates INTEGER NOT NULL,
                claim_successor INTEGER NOT NULL DEFAULT 0,
                spent_height INTEGER
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO registry_account (id, ufvk, network, birthday, sync_height, sync_hash)
             VALUES (0, 'ufvk', 'test', 90, 100, ?1)",
            params![vec![1u8; 32]],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO names (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, memo)
             VALUES ('z', 1, 'claim', 'u', 'none', x'00', x'00', x'00', x'00', x'00', x'00', 0, 0, x'00')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO anchor_facts (nullifier, value, height, tx_index, action_index, name_note_candidates, claim_successor, spent_height)
             VALUES (x'01', 0, 90, 0, 0, 0, 0, NULL)",
            [],
        )
        .unwrap();

        crate::registry::storage::install_schema(&conn).unwrap();

        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 5);
        let birthday: i64 = conn
            .query_row(
                "SELECT birthday FROM registry_account WHERE id = 0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(birthday, 90);
        assert!(checkpoint(&conn).unwrap().is_none());
        for table in ["names", "anchor_facts"] {
            let rows: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(rows, 0, "{table}");
        }
    }

    /// A version 4 database stored a successor for a claim that was not
    /// admitted. A later claim may have spent that successor. Opening it
    /// drops the scan and keeps the account, so the next run replays from
    /// the birthday.
    #[test]
    fn version_4_database_rescans_from_the_birthday() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "PRAGMA user_version = 4;
             CREATE TABLE registry_account (
                id INTEGER NOT NULL PRIMARY KEY CHECK (id = 0),
                ufvk TEXT NOT NULL,
                network TEXT NOT NULL,
                birthday INTEGER NOT NULL,
                sync_height INTEGER,
                sync_hash BLOB
             );
             CREATE TABLE name_events (
                name TEXT NOT NULL,
                height INTEGER NOT NULL,
                action TEXT NOT NULL,
                ua TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                prev_rcm BLOB NOT NULL,
                rcm BLOB NOT NULL,
                psi BLOB NOT NULL,
                cmx BLOB NOT NULL,
                nullifier BLOB NOT NULL,
                txid BLOB NOT NULL,
                tx_index INTEGER NOT NULL,
                action_index INTEGER NOT NULL,
                memo BLOB NOT NULL,
                PRIMARY KEY (name, height, txid, action_index)
             );
             CREATE TABLE anchor_facts (
                nullifier BLOB NOT NULL PRIMARY KEY,
                value INTEGER NOT NULL,
                height INTEGER NOT NULL,
                tx_index INTEGER NOT NULL,
                action_index INTEGER NOT NULL,
                name_note_candidates INTEGER NOT NULL,
                claim_successor INTEGER NOT NULL DEFAULT 0,
                spent_height INTEGER
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO registry_account (id, ufvk, network, birthday, sync_height, sync_hash)
             VALUES (0, 'ufvk', 'test', 90, 100, ?1)",
            params![vec![1u8; 32]],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO name_events (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, memo)
             VALUES ('alice', 10, 'claim', 'u', 'none', x'00', x'00', x'00', x'00', x'11', x'22', 0, 0, x'00')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO name_events (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, memo)
             VALUES ('bob', 20, 'claim', 'u', 'none', x'00', x'00', x'00', x'00', x'33', x'44', 0, 0, x'00')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO anchor_facts (nullifier, value, height, tx_index, action_index, name_note_candidates, claim_successor, spent_height)
             VALUES (x'11', 0, 10, 0, 1, 1, 1, NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO anchor_facts (nullifier, value, height, tx_index, action_index, name_note_candidates, claim_successor, spent_height)
             VALUES (x'22', 0, 12, 0, 1, 1, 1, 20)",
            [],
        )
        .unwrap();

        crate::registry::storage::install_schema(&conn).unwrap();

        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 5);
        let birthday: i64 = conn
            .query_row(
                "SELECT birthday FROM registry_account WHERE id = 0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(birthday, 90);
        assert!(checkpoint(&conn).unwrap().is_none());
        for table in ["name_events", "anchor_facts"] {
            let rows: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(rows, 0, "{table}");
        }
    }

    #[test]
    fn mtp_is_the_median_of_eleven_block_times() {
        let conn = database();
        for height in 1..=11 {
            conn.execute(
                "INSERT INTO block_times (height, time) VALUES (?1, ?2)",
                params![height, height * 10],
            )
            .unwrap();
        }
        assert_eq!(mtp_at(&conn, 11).unwrap(), Some(60));
        assert_eq!(mtp_at(&conn, 10).unwrap(), None);
    }

    fn insert_checkpoint(conn: &Connection, height: u32, hash_byte: u8) {
        conn.execute(
            "UPDATE registry_account SET sync_height = ?1, sync_hash = ?2 WHERE id = 0",
            params![height as i64, vec![hash_byte; 32]],
        )
        .unwrap();
    }

    #[test]
    fn checkpoint_persists_the_sync_position() {
        let conn = database();
        conn.execute(
            "INSERT INTO registry_account (id, ufvk, network, birthday) VALUES (0, 'ufvk', 'test', 1)",
            [],
        )
        .unwrap();
        insert_checkpoint(&conn, 42, 7);

        let scanned = checkpoint(&conn).unwrap().unwrap();
        assert_eq!(scanned.height, BlockHeight::from_u32(42));
        assert_eq!(scanned.hash.0, [7; 32]);
    }

    #[test]
    fn rewind_resets_the_sync_position_to_the_fork_height() {
        let conn = database();
        conn.execute(
            "INSERT INTO registry_account (id, ufvk, network, birthday) VALUES (0, 'ufvk', 'test', 1)",
            [],
        )
        .unwrap();
        insert_checkpoint(&conn, 42, 7);
        conn.execute(
            "INSERT INTO anchor_facts (nullifier, value, height, tx_index, action_index, name_note_candidates, spent_height)
             VALUES (?1, 0, 42, 0, 0, 0, NULL)",
            params![vec![1u8; 32]],
        )
        .unwrap();

        rewind(&conn, 41).unwrap();

        let position = checkpoint(&conn).unwrap();
        assert!(position.is_none()); // NULL hash: a restart replays from the birthday.
        let watched: u64 = conn
            .query_row("SELECT COUNT(*) FROM anchor_facts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(watched, 0);
    }

    /// The block before the fork stays, including a spend in it. Replaying
    /// that event leaves the tip where it stands. The fork itself is dropped.
    #[test]
    fn rewind_keeps_the_seam_block_and_replay_leaves_the_tip() {
        let conn = database();
        conn.execute(
            "INSERT INTO registry_account (id, ufvk, network, birthday) VALUES (0, 'ufvk', 'test', 1)",
            [],
        )
        .unwrap();
        insert_checkpoint(&conn, 50, 7);

        let memo =
            b"ZNS:claim:z:u:none:0000000000000000000000000000000000000000000000000000000000000000";
        for (height, txid) in [(39_i64, 1_u8), (40, 2), (41, 3)] {
            conn.execute(
                "INSERT INTO name_events (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, memo)
                 VALUES ('z', ?1, 'claim', 'u', 'none', x'00', x'02', x'03', x'04', x'5a', ?2, 0, 0, ?3)",
                params![height, vec![txid; 32], &memo[..]],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO names (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, memo)
             VALUES ('z', 41, 'claim', 'u', 'none', x'00', x'02', x'03', x'04', x'5a', ?1, 0, 0, ?2)",
            params![vec![3_u8; 32], &memo[..]],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO anchor_facts (nullifier, value, height, tx_index, action_index, name_note_candidates, spent_height, spent_tx_index, spent_action_index)
             VALUES (?1, 0, 30, 0, 0, 0, 40, 0, 0)",
            params![vec![9_u8; 32]],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO anchor_facts (nullifier, value, height, tx_index, action_index, name_note_candidates, spent_height)
             VALUES (?1, 0, 40, 0, 0, 0, NULL)",
            params![vec![8_u8; 32]],
        )
        .unwrap();

        rewind(&conn, 41).unwrap();

        let heights: Vec<i64> = {
            let mut stmt = conn
                .prepare("SELECT height FROM name_events ORDER BY height")
                .unwrap();
            stmt.query_map([], |row| row.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(heights, vec![39, 40]);
        let tip_height: i64 = conn
            .query_row("SELECT height FROM names WHERE name = 'z'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(tip_height, 40);
        let spent: Option<i64> = conn
            .query_row(
                "SELECT spent_height FROM anchor_facts WHERE nullifier = ?1",
                params![vec![9_u8; 32]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(spent, Some(40));
        let seam_facts: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM anchor_facts WHERE nullifier = ?1",
                params![vec![8_u8; 32]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(seam_facts, 1);

        let tx = conn.unchecked_transaction().unwrap();
        let replayed = record_admission(
            &tx,
            &AdmissionRow {
                name: "z",
                height: 40,
                action: "claim",
                ua: "u",
                expires_at: "none",
                prev_rcm: &[0],
                rcm: &[2],
                psi: &[3],
                cmx: &[4],
                nullifier: &[0x5a],
                txid: &[2; 32],
                tx_index: 0,
                action_index: 0,
                confirmed_mtp: None,
                memo,
            },
        )
        .unwrap();
        assert!(!replayed);
        tx.commit().unwrap();

        let tip_after: i64 = conn
            .query_row("SELECT height FROM names WHERE name = 'z'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(tip_after, 40);
        let events: i64 = conn
            .query_row("SELECT COUNT(*) FROM name_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(events, 2);
    }

    /// The live-name set claimed a nullifier the names table does not have.
    /// That used to panic under the database lock.
    #[test]
    fn mark_released_errors_when_the_names_row_is_missing() {
        let conn = database();
        let tx = conn.unchecked_transaction().unwrap();
        let nf = [0x5a_u8; 32];
        let mut live_names = HashSet::from([TipNf::from_bytes(&nf)]);
        let facts = TxAnchorFacts {
            adoptions: vec![],
            retirements: vec![Retirement {
                nf: AnchorNf::from_bytes(&nf),
            }],
            has_single_name_note: false,
            name_notes: 0,
            claim_successor: false,
        };

        let error =
            mark_released_spent(&tx, &mut live_names, &facts, 100, &[1; 32], 0, 0).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("live-name set does not match the names table"),
            "{error}"
        );
        assert!(live_names.contains(&TipNf::from_bytes(&nf)));
        let releases: i64 = tx
            .query_row("SELECT COUNT(*) FROM implicit_releases", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(releases, 0);
    }

    fn account(conn: &Connection) -> (String, String, i64) {
        conn.query_row(
            "SELECT ufvk, network, birthday FROM registry_account WHERE id = 0",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
    }

    #[test]
    fn unscanned_registry_takes_a_new_configuration() {
        let conn = database();
        install_registry_config(&conn, "old", "test", 1).unwrap();
        install_registry_config(&conn, "new", "main", 9).unwrap();
        assert_eq!(account(&conn), ("new".into(), "main".into(), 9));
    }

    #[test]
    fn scanned_registry_refuses_a_different_ufvk_or_network() {
        let conn = database();
        install_registry_config(&conn, "ufvk", "test", 1).unwrap();
        conn.execute(
            "INSERT INTO anchor_facts (nullifier, value, height, tx_index, action_index, name_note_candidates, spent_height)
             VALUES (?1, 0, 10, 0, 0, 0, NULL)",
            params![vec![1u8; 32]],
        )
        .unwrap();

        let different_key = install_registry_config(&conn, "other", "test", 1).unwrap_err();
        assert!(
            different_key
                .to_string()
                .contains("different ufvk or network"),
            "{different_key}"
        );
        let different_network = install_registry_config(&conn, "ufvk", "main", 1).unwrap_err();
        assert!(
            different_network
                .to_string()
                .contains("different ufvk or network"),
            "{different_network}"
        );
        assert_eq!(account(&conn).0, "ufvk");
        assert_eq!(account(&conn).1, "test");
    }

    #[test]
    fn scanned_registry_keeps_its_birthday() {
        let conn = database();
        install_registry_config(&conn, "ufvk", "test", 10).unwrap();
        conn.execute(
            "INSERT INTO anchor_facts (nullifier, value, height, tx_index, action_index, name_note_candidates, spent_height)
             VALUES (?1, 0, 10, 0, 0, 0, NULL)",
            params![vec![1u8; 32]],
        )
        .unwrap();

        install_registry_config(&conn, "ufvk", "test", 99).unwrap();
        assert_eq!(account(&conn).2, 10);
    }

    #[test]
    fn lineage_ignores_payment_notes() {
        let conn = database();
        for action in 0..40 {
            let mut nf = [0u8; 32];
            nf[0] = action as u8 + 1;
            conn.execute(
                "INSERT INTO anchor_facts (nullifier, value, height, tx_index, action_index, name_note_candidates, spent_height)
                 VALUES (?1, 0, 10, 0, ?2, 0, NULL)",
                params![nf.to_vec(), action],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO anchor_facts (nullifier, value, height, tx_index, action_index, name_note_candidates, spent_height)
             VALUES (?1, 5, 11, 0, 0, 0, NULL)",
            params![vec![2u8; 32]],
        )
        .unwrap();

        let lineage = fold_lineage(lineage_facts(&conn).unwrap());
        assert_eq!(lineage.len(), 40);
        assert!(lineage.adoption_closed());
        assert_eq!(ironwood_nullifiers(&conn).unwrap().len(), 40);
        assert_eq!(drop_payment_notes(&conn).unwrap(), 1);
        let left: i64 = conn
            .query_row("SELECT COUNT(*) FROM anchor_facts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(left, 40);
    }

    /// One stored zero-value note is not the keygen transaction, so the
    /// fold does not adopt it.
    #[test]
    fn a_stored_note_outside_the_keygen_transaction_does_not_join() {
        let conn = database();
        conn.execute(
            "INSERT INTO anchor_facts (nullifier, value, height, tx_index, action_index, name_note_candidates, spent_height)
             VALUES (?1, 0, 10, 0, 0, 0, NULL)",
            params![vec![1u8; 32]],
        )
        .unwrap();

        let lineage = fold_lineage(lineage_facts(&conn).unwrap());
        assert!(lineage.is_empty());
        assert!(!lineage.adoption_closed());
    }

    /// A stored adoption, the spend that retires it, and the successor note
    /// fold to the pool the batch would keep. The pool is already full, so
    /// the successor enters only because the stored row says it is a claim
    /// successor.
    #[test]
    fn lineage_facts_replay_a_spent_anchor_and_its_successor() {
        let conn = database();
        let mut spent = [0u8; 32];
        spent[0] = 1;
        for seed in 1..=40 {
            let mut nullifier = [0u8; 32];
            nullifier[0] = seed;
            conn.execute(
                "INSERT INTO anchor_facts (nullifier, value, height, tx_index, action_index, name_note_candidates, claim_successor, spent_height)
                 VALUES (?1, 0, 10, 0, ?2, 0, 0, NULL)",
                params![nullifier.as_slice(), i64::from(seed) - 1],
            )
            .unwrap();
        }
        conn.execute(
            "UPDATE anchor_facts SET spent_height = 12, spent_tx_index = 0, spent_action_index = 0
             WHERE nullifier = ?1",
            params![spent.as_slice()],
        )
        .unwrap();
        let mut successor = [0u8; 32];
        successor[0] = 200;
        conn.execute(
            "INSERT INTO anchor_facts (nullifier, value, height, tx_index, action_index, name_note_candidates, claim_successor, spent_height)
             VALUES (?1, 0, 12, 0, 1, 1, 1, NULL)",
            params![successor.as_slice()],
        )
        .unwrap();

        let lineage = fold_lineage(lineage_facts(&conn).unwrap());

        assert!(lineage.adoption_closed());
        assert_eq!(lineage.len(), 40);
        assert!(lineage.contains(&AnchorNf::from_bytes(&successor)));
        assert!(!lineage.contains(&AnchorNf::from_bytes(&spent)));
    }

    #[test]
    fn a_fact_height_that_does_not_fit_fails_the_snapshot() {
        let conn = database();
        conn.execute(
            "INSERT INTO anchor_facts (nullifier, value, height, tx_index, action_index, name_note_candidates, spent_height)
             VALUES (?1, 0, -1, 0, 0, 0, NULL)",
            params![vec![1u8; 32]],
        )
        .unwrap();

        let error = lineage_facts(&conn).unwrap_err();
        assert!(
            error.to_string().contains("registry record is corrupt"),
            "{error}"
        );
    }

    #[test]
    fn events_include_an_implicit_release() {
        let conn = database();
        let memo =
            b"ZNS:claim:z:u:none:0000000000000000000000000000000000000000000000000000000000000000";
        conn.execute(
            "INSERT INTO name_events (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, memo)
             VALUES ('z', 90, 'claim', 'u', 'none', x'00', x'02', x'03', x'04', x'5a', ?1, 0, 0, ?2)",
            params![vec![6u8; 32], &memo[..]],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO implicit_releases (name, height, txid, tx_index, action_index, nullifier)
             VALUES ('z', 100, ?1, 1, 2, x'5a')",
            params![vec![7u8; 32]],
        )
        .unwrap();

        let (log, total) = events(&conn, None, None, None, 10, 0).unwrap();
        assert_eq!(total, 2);
        assert_eq!(log[0].action, Action::Release);
        assert_eq!(log[0].height, 100);
        assert_eq!(log[0].action_index, 2);
        assert_eq!(log[0].name, "z");
        assert_eq!(log[0].ua, "u");
        assert_eq!(log[0].expires_at, "none");
        assert!(log[0].id < 0);
        assert_eq!(log[0].txid, [7u8; 32]);
        assert_eq!(log[1].action, Action::Claim);
        assert_eq!(log[1].height, 90);

        let (claims, claim_total) =
            events(&conn, Some("z"), Some(Action::Claim), None, 10, 0).unwrap();
        assert_eq!(claim_total, 1);
        assert_eq!(claims[0].action, Action::Claim);
    }

    #[test]
    fn tied_implicit_releases_page_by_id() {
        let conn = database();
        let prev = "0000000000000000000000000000000000000000000000000000000000000000";
        for name in ["a", "b"] {
            let memo = format!("ZNS:claim:{name}:u:none:{prev}");
            conn.execute(
                "INSERT INTO name_events (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, memo)
                 VALUES (?1, 90, 'claim', 'u', 'none', x'00', x'02', x'03', x'04', ?2, ?3, 0, 0, ?4)",
                params![name, name.as_bytes(), vec![6u8; 32], memo.as_bytes()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO implicit_releases (name, height, txid, tx_index, action_index, nullifier)
                 VALUES (?1, 100, ?2, 1, 2, ?3)",
                params![name, vec![7u8; 32], name.as_bytes()],
            )
            .unwrap();
        }

        let mut paged = Vec::new();
        for offset in 0..4 {
            let (page, total) = events(&conn, None, None, None, 1, offset).unwrap();
            assert_eq!(total, 4);
            assert_eq!(page.len(), 1);
            paged.push(page[0].id);
        }
        let (all, _) = events(&conn, None, None, None, 4, 0).unwrap();
        assert_eq!(paged, all.iter().map(|event| event.id).collect::<Vec<_>>());
        assert_eq!(all[0].height, 100);
        assert_eq!(all[1].height, 100);
        assert!(all[0].id > all[1].id);
    }
}

#[cfg(test)]
mod admission {
    use super::*;
    use crate::registry::storage::SCHEMA_SQL;
    use orchard::keys::SpendingKey;
    use orchard::note::NoteVersion;
    use zcash_address::unified;
    use zcash_protocol::consensus::NetworkType;
    use zns_verify::zns_psi_rcm;

    const RAW_ADDR: [u8; 43] = [
        7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];

    fn fvk() -> FullViewingKey {
        FullViewingKey::from(&SpendingKey::from_bytes([7u8; 32]).expect("spending key"))
    }

    /// An orchard-only unified address for the active network.
    fn test_ua() -> String {
        let network = if cfg!(feature = "testnet") {
            NetworkType::Test
        } else {
            NetworkType::Main
        };
        unified::Address::try_from_items(vec![unified::Receiver::Orchard([0x03; 43])])
            .expect("orchard-only UA")
            .encode(&network)
    }

    fn memo_for(action: &str, name: &str, prev: &[u8; 32]) -> Memo {
        Memo::from_bytes(format!("ZNS:{action}:{name}:{}:none:{}", test_ua(), hex(prev)).as_bytes())
            .expect("memo")
    }

    fn hex(bytes: &[u8; 32]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn nf(seed: u8) -> AnchorNf {
        let mut bytes = [0u8; 32];
        bytes[0] = seed;
        AnchorNf::from_bytes(&bytes)
    }

    /// A zero-value note addressed to the test recipient.
    fn test_note() -> Note {
        use orchard::note::{RandomSeed, Rho};
        use orchard::value::NoteValue;
        let recipient = orchard::Address::from_raw_address_bytes(&RAW_ADDR)
            .into_option()
            .expect("address");
        let rho = Rho::from_bytes(&[9u8; 32]).into_option().expect("rho");
        let rseed = RandomSeed::from_bytes([4u8; 32], &rho)
            .into_option()
            .expect("rseed");
        Note::from_parts(recipient, NoteValue::ZERO, rho, rseed, NoteVersion::V3)
            .into_option()
            .expect("note")
    }

    /// Builds a verified candidate the same way triage does.
    #[allow(clippy::too_many_arguments)] // a fixture; grouping the args hides the shape
    fn candidate<'a>(
        memos: &'a [Memo],
        i: usize,
        _tx_index: u32,
        action_index: usize,
        txid: [u8; 32],
        action: &str,
        name: &str,
        prev: [u8; 32],
    ) -> Candidate<'a> {
        let ua = test_ua();
        let stored = &memos[i];
        let note = NameNote::parse(stored).expect("parsed");
        let memo_bytes = stored.text().expect("memo is utf-8").as_bytes();

        let rho = zns_verify::Rho::from_bytes(&[9u8; 32]).expect("rho");
        let (psi, rcm) = zns_psi_rcm(
            action.as_bytes(),
            name.as_bytes(),
            ua.as_bytes(),
            b"none",
            &prev,
        );
        let diversifier: [u8; 11] = RAW_ADDR[..11].try_into().expect("diversifier");
        let g_d = notes::diversify_hash(&diversifier);
        let pk_d: [u8; 32] = RAW_ADDR[11..].try_into().expect("pk_d");
        let cmx = zns_verify::note_commitment_cmx(g_d, pk_d, 0, rho, psi, rcm).expect("commitment");

        Candidate {
            action_index,
            txid,
            memo: memo_bytes,
            note,
            cand_cmx: cmx.to_bytes(),
            note_orchard: test_note(),
            psi,
            rcm,
            fail_nullifier: false,
        }
    }

    fn tx_law_for(
        txid: [u8; 32],
        snapshot: Lineage,
        adoptions: Vec<Adoption>,
        retirements: Vec<Retirement>,
        has_single_name_note: bool,
    ) -> TxLaw {
        let claim_successor = has_single_name_note && adoptions.len() == 1;
        TxLaw {
            txid,
            tx_index: 0,
            snapshot,
            facts: TxAnchorFacts {
                adoptions,
                retirements,
                has_single_name_note,
                name_notes: usize::from(has_single_name_note),
                claim_successor,
            },
        }
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA_SQL).unwrap();
        conn
    }

    #[test]
    fn backed_claim_binds_the_name() {
        let conn = db();
        let tx = conn.unchecked_transaction().unwrap();
        let fvk = fvk();

        let memos = vec![memo_for("claim", "alice", &[0u8; 32])];
        let candidates = vec![candidate(
            &memos, 0, 0, 0, [1; 32], "claim", "alice", [0u8; 32],
        )];
        let snapshot = Lineage::new();
        let seeded = seeded_lineage(&[1]);
        let tx_law = vec![tx_law_for(
            [1; 32],
            seeded.clone(),
            vec![Adoption { nf: nf(200) }],
            vec![Retirement { nf: nf(1) }],
            true,
        )];
        let mut live_names: HashSet<TipNf> = HashSet::new();
        let mut lineage = seeded;
        let _ = snapshot;

        // the fold already consumed the block's facts before admission
        lineage.step_tx(100, &tx_law[0].facts);

        apply_candidates(&tx, 100, None, &candidates, &mut live_names, &tx_law, &fvk).unwrap();

        assert_eq!(count(&tx, "SELECT COUNT(*) FROM names"), 1);
        assert_eq!(count(&tx, "SELECT COUNT(*) FROM name_events"), 1);
        assert_eq!(count(&tx, "SELECT COUNT(*) FROM implicit_releases"), 0);
    }

    /// A second claim for a name that already has a live record does not
    /// replace that record.
    #[test]
    fn a_second_claim_leaves_the_first_name_in_place() {
        let conn = db();
        let tx = conn.unchecked_transaction().unwrap();
        let fvk = fvk();

        let memos = vec![memo_for("claim", "alice", &[0u8; 32])];
        let first = vec![candidate(
            &memos, 0, 0, 0, [1; 32], "claim", "alice", [0u8; 32],
        )];
        let seeded = seeded_lineage(&[1]);
        let first_law = tx_law_for(
            [1; 32],
            seeded.clone(),
            vec![Adoption { nf: nf(200) }],
            vec![Retirement { nf: nf(1) }],
            true,
        );
        let mut live_names: HashSet<TipNf> = HashSet::new();
        let mut lineage = seeded;
        lineage.step_tx(100, &first_law.facts);
        apply_candidates(&tx, 100, None, &first, &mut live_names, &[first_law], &fvk).unwrap();

        let second_memo = vec![memo_for("claim", "alice", &[0u8; 32])];
        let second = vec![candidate(
            &second_memo,
            0,
            0,
            0,
            [2; 32],
            "claim",
            "alice",
            [0u8; 32],
        )];
        let mut second_law = tx_law_for(
            [2; 32],
            lineage.clone(),
            vec![Adoption { nf: nf(201) }],
            vec![Retirement { nf: nf(200) }],
            true,
        );
        // The name is already live, so the successor stays out. The anchor
        // it spent still retires.
        second_law.facts.claim_successor = false;
        lineage.step_tx(101, &second_law.facts);
        assert!(!lineage.contains(&nf(201)));
        assert!(!lineage.contains(&nf(200)));
        apply_candidates(
            &tx,
            101,
            None,
            &second,
            &mut live_names,
            &[second_law],
            &fvk,
        )
        .unwrap();

        assert_eq!(count(&tx, "SELECT COUNT(*) FROM names"), 1);
        assert_eq!(count(&tx, "SELECT COUNT(*) FROM name_events"), 1);
        let height: i64 = tx
            .query_row("SELECT height FROM names WHERE name = 'alice'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(height, 100);
    }

    /// An update that also spends a live anchor does not renew the name.
    /// The spent tip ends the binding.
    #[test]
    fn an_update_that_spends_an_anchor_ends_the_binding() {
        let conn = db();
        let tx = conn.unchecked_transaction().unwrap();
        let fvk = fvk();

        let memos = vec![memo_for("claim", "alice", &[0u8; 32])];
        let claim = vec![candidate(
            &memos, 0, 0, 0, [1; 32], "claim", "alice", [0u8; 32],
        )];
        let seeded = seeded_lineage(&[1]);
        let claim_law = tx_law_for(
            [1; 32],
            seeded.clone(),
            vec![Adoption { nf: nf(200) }],
            vec![Retirement { nf: nf(1) }],
            true,
        );
        let mut live_names: HashSet<TipNf> = HashSet::new();
        let mut lineage = seeded;
        lineage.step_tx(100, &claim_law.facts);
        apply_candidates(&tx, 100, None, &claim, &mut live_names, &[claim_law], &fvk).unwrap();

        let tip: Vec<u8> = tx
            .query_row(
                "SELECT nullifier FROM names WHERE name = 'alice'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let tip: [u8; 32] = tip.try_into().unwrap();
        let update_memo = vec![memo_for("update", "alice", &[0x02; 32])];
        let update = vec![candidate(
            &update_memo,
            0,
            0,
            0,
            [3; 32],
            "update",
            "alice",
            [0x02; 32],
        )];
        let update_law = tx_law_for(
            [3; 32],
            lineage,
            vec![],
            vec![
                Retirement {
                    nf: AnchorNf::from_bytes(&tip),
                },
                Retirement { nf: nf(200) },
            ],
            true,
        );
        apply_candidates(
            &tx,
            101,
            None,
            &update,
            &mut live_names,
            &[update_law],
            &fvk,
        )
        .unwrap();

        assert_eq!(count(&tx, "SELECT COUNT(*) FROM names"), 0);
        assert_eq!(
            count(
                &tx,
                "SELECT COUNT(*) FROM name_events WHERE action = 'update'"
            ),
            0
        );
        assert_eq!(count(&tx, "SELECT COUNT(*) FROM implicit_releases"), 1);
    }

    /// A second registry output means the mint's successor is absent, so
    /// the claim does not bind even though one zero-value note is present.
    #[test]
    fn claim_with_an_extra_registry_output_does_not_bind() {
        let conn = db();
        let tx = conn.unchecked_transaction().unwrap();
        let fvk = fvk();

        let memos = vec![memo_for("claim", "alice", &[0u8; 32])];
        let candidates = vec![candidate(
            &memos, 0, 0, 0, [1; 32], "claim", "alice", [0u8; 32],
        )];
        let seeded = seeded_lineage(&[1]);
        let tx_law = vec![TxLaw {
            txid: [1; 32],
            tx_index: 0,
            snapshot: seeded,
            facts: TxAnchorFacts {
                adoptions: vec![Adoption { nf: nf(200) }],
                retirements: vec![Retirement { nf: nf(1) }],
                has_single_name_note: true,
                name_notes: 1,
                claim_successor: false,
            },
        }];
        let mut live_names: HashSet<TipNf> = HashSet::new();

        apply_candidates(&tx, 100, None, &candidates, &mut live_names, &tx_law, &fvk).unwrap();

        assert_eq!(count(&tx, "SELECT COUNT(*) FROM names"), 0);
        assert_eq!(count(&tx, "SELECT COUNT(*) FROM name_events"), 0);
    }

    /// A released name's claim event is still in the log. Replaying that
    /// claim (a birthday rescan, the name free again) must not abort and
    /// must not put the name back.
    #[test]
    fn replaying_a_recorded_claim_leaves_a_free_name_free() {
        let conn = db();
        let tx = conn.unchecked_transaction().unwrap();
        let fvk = fvk();

        tx.execute(
            "INSERT INTO name_events (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, memo)
             VALUES ('alice', 100, 'claim', 'u', 'none', x'00', x'02', x'03', x'04', x'05', ?1, 0, 0, x'07')",
            params![vec![1_u8; 32]],
        )
        .unwrap();

        let memos = vec![memo_for("claim", "alice", &[0u8; 32])];
        let candidates = vec![candidate(
            &memos, 0, 0, 0, [1; 32], "claim", "alice", [0u8; 32],
        )];
        let seeded = seeded_lineage(&[1]);
        let tx_law = vec![tx_law_for(
            [1; 32],
            seeded,
            vec![Adoption { nf: nf(200) }],
            vec![Retirement { nf: nf(1) }],
            true,
        )];
        let mut live_names = HashSet::new();

        apply_candidates(&tx, 100, None, &candidates, &mut live_names, &tx_law, &fvk).unwrap();

        assert!(live_names.is_empty());
        assert_eq!(count(&tx, "SELECT COUNT(*) FROM names"), 0);
        assert_eq!(count(&tx, "SELECT COUNT(*) FROM name_events"), 1);
    }

    /// The mint's builder spends the anchor in one action and puts the name
    /// note in another, with fee inputs between them. `accept_claim` still
    /// binds the name: exactly one live anchor retired, one successor.
    #[test]
    fn claim_binds_when_the_anchor_spend_is_a_different_action() {
        let conn = db();
        let tx = conn.unchecked_transaction().unwrap();
        let fvk = fvk();

        let memos = vec![memo_for("claim", "julian", &[0u8; 32])];
        let candidates = vec![candidate(
            &memos, 0, 0, 0, [2; 32], "claim", "julian", [0u8; 32],
        )];
        let seeded = seeded_lineage(&[1]);
        let tx_law = vec![tx_law_for(
            [2; 32],
            seeded.clone(),
            vec![Adoption { nf: nf(200) }],
            vec![Retirement { nf: nf(1) }, Retirement { nf: nf(9) }],
            true,
        )];
        let mut live_names = HashSet::new();

        apply_candidates(&tx, 100, None, &candidates, &mut live_names, &tx_law, &fvk).unwrap();

        assert_eq!(count(&tx, "SELECT COUNT(*) FROM names"), 1);
        assert_eq!(
            tx.query_row("SELECT name FROM names", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "julian"
        );
    }

    #[test]
    fn unbacked_claim_is_rejected_without_state_change() {
        let conn = db();
        let tx = conn.unchecked_transaction().unwrap();
        let fvk = fvk();

        let memos = vec![memo_for("claim", "alice", &[0u8; 32])];
        let candidates = vec![candidate(
            &memos, 0, 0, 0, [1; 32], "claim", "alice", [0u8; 32],
        )];
        let seeded = seeded_lineage(&[1]);
        let tx_law = vec![tx_law_for(
            [1; 32],
            seeded.clone(),
            vec![],
            vec![Retirement {
                nf: AnchorNf::from_bytes(&[9; 32]),
            }],
            true,
        )];
        let mut live_names = HashSet::new();
        let mut lineage = seeded;
        lineage.step_tx(100, &tx_law[0].facts);

        apply_candidates(&tx, 100, None, &candidates, &mut live_names, &tx_law, &fvk).unwrap();

        assert_eq!(count(&tx, "SELECT COUNT(*) FROM names"), 0);
        assert_eq!(count(&tx, "SELECT COUNT(*) FROM implicit_releases"), 0);
    }

    /// Review finding 1, the flagged shape: a claim whose transaction spends
    /// another name's live tip. The mint's mark_released ends that binding
    /// and the claim is rejected.
    #[test]
    fn claim_spending_a_live_name_tip_ends_that_binding() {
        let conn = db();
        let tx = conn.unchecked_transaction().unwrap();
        let fvk = fvk();

        // "z" is live, its tip nullifier is 0x5a…
        conn.execute(
            "INSERT INTO names (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, confirmed_mtp, memo)
             VALUES ('z', 90, 'claim', 'u', 'none', x'00', x'0202020202020202020202020202020202020202020202020202020202020202', x'03', x'04', ?1, x'06', 0, 0, 0, x'07')",
            params![vec![0x5a_u8; 32]],
        )
        .unwrap();

        let memos = vec![memo_for("claim", "a", &[0u8; 32])];
        let candidates = vec![candidate(&memos, 0, 0, 0, [1; 32], "claim", "a", [0u8; 32])];
        let seeded = seeded_lineage(&[1]);
        let tx_law = vec![tx_law_for(
            [1; 32],
            seeded.clone(),
            vec![],
            vec![Retirement {
                nf: AnchorNf::from_bytes(&[0x5a; 32]),
            }],
            true,
        )];
        let mut live_names: HashSet<TipNf> = HashSet::from([TipNf::from_bytes(&[0x5a; 32])]);
        let mut lineage = seeded;
        lineage.step_tx(100, &tx_law[0].facts);

        apply_candidates(&tx, 100, None, &candidates, &mut live_names, &tx_law, &fvk).unwrap();

        // z's binding ended; the claim did not land.
        assert_eq!(count(&tx, "SELECT COUNT(*) FROM names"), 0);
        assert_eq!(count(&tx, "SELECT COUNT(*) FROM implicit_releases"), 1);
        let (rel_name, rel_nf): (String, Vec<u8>) = tx
            .query_row("SELECT name, nullifier FROM implicit_releases", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(rel_name, "z");
        assert_eq!(rel_nf, vec![0x5a; 32]);
    }

    /// The update-then-release shape in canonical order: the update lands,
    /// and a stale-predecessor release ends the binding (the mint's
    /// release_predecessor) — the ordering the release-first partition used
    /// to emulate.
    #[test]
    fn update_then_stale_release_ends_the_binding() {
        let conn = db();
        let tx = conn.unchecked_transaction().unwrap();
        let fvk = fvk();

        // "z" is live with tip nullifier 0x5a and commitment rcm 0x02.
        conn.execute(
            "INSERT INTO names (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, confirmed_mtp, memo)
             VALUES ('z', 90, 'claim', 'u', 'none', x'00', x'0202020202020202020202020202020202020202020202020202020202020202', x'03', x'04', ?1, x'06', 0, 0, 0, x'07')",
            params![vec![0x5a_u8; 32]],
        )
        .unwrap();

        // tx 1: the update consumes the tip. tx 2: the release consumes the
        // update's tip but discloses the stale predecessor.
        let memos = vec![
            memo_for("update", "z", &[0x02; 32]),
            memo_for("release", "z", &[0x02; 32]),
        ];
        let update = candidate(&memos, 0, 0, 0, [1; 32], "update", "z", [0x02; 32]);
        let release = candidate(&memos, 1, 0, 0, [2; 32], "release", "z", [0x02; 32]);
        let nullifier_u = admit_nullifier(&update, &fvk).expect("update nullifier");

        let seeded = seeded_lineage(&[1]);
        let tx_law = vec![
            tx_law_for(
                [1; 32],
                seeded.clone(),
                vec![],
                vec![Retirement {
                    nf: AnchorNf::from_bytes(&[0x5a; 32]),
                }],
                true,
            ),
            tx_law_for(
                [2; 32],
                seeded.clone(),
                vec![],
                vec![Retirement {
                    nf: AnchorNf::from_bytes(nullifier_u.as_bytes()),
                }],
                true,
            ),
        ];

        let mut live_names: HashSet<TipNf> = HashSet::from([TipNf::from_bytes(&[0x5a; 32])]);
        let mut lineage = seeded;
        lineage.step_tx(100, &tx_law[0].facts);
        lineage.step_tx(100, &tx_law[1].facts);

        let candidates = vec![update, release];
        apply_candidates(&tx, 100, None, &candidates, &mut live_names, &tx_law, &fvk).unwrap();

        // The update admitted; the stale release ended the binding.
        assert_eq!(count(&tx, "SELECT COUNT(*) FROM names"), 0);
        assert_eq!(count(&tx, "SELECT COUNT(*) FROM implicit_releases"), 1);
        assert_eq!(count(&tx, "SELECT COUNT(*) FROM name_events"), 1);
        let (rel_name, rel_nf): (String, Vec<u8>) = tx
            .query_row("SELECT name, nullifier FROM implicit_releases", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(rel_name, "z");
        assert_eq!(rel_nf.as_slice(), nullifier_u.as_bytes());
    }

    /// The tip was spent and the chain rule passed, but the successor
    /// nullifier cannot be derived. The binding ends; the spent name does
    /// not stay resolvable.
    #[test]
    fn update_whose_nullifier_cannot_be_derived_ends_the_binding() {
        let conn = db();
        let tx = conn.unchecked_transaction().unwrap();
        let fvk = fvk();

        conn.execute(
            "INSERT INTO names (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, memo)
             VALUES ('z', 90, 'claim', 'u', 'none', x'00', x'0202020202020202020202020202020202020202020202020202020202020202', x'03', x'04', ?1, x'06', 0, 0, x'07')",
            params![vec![0x5a_u8; 32]],
        )
        .unwrap();

        let memos = vec![memo_for("update", "z", &[0x02; 32])];
        let mut update = candidate(&memos, 0, 0, 0, [1; 32], "update", "z", [0x02; 32]);
        update.fail_nullifier = true;
        assert!(admit_nullifier(&update, &fvk).is_none());

        let seeded = seeded_lineage(&[1]);
        let tx_law = vec![tx_law_for(
            [1; 32],
            seeded,
            vec![],
            vec![Retirement {
                nf: AnchorNf::from_bytes(&[0x5a; 32]),
            }],
            true,
        )];
        let mut live_names: HashSet<TipNf> = HashSet::from([TipNf::from_bytes(&[0x5a; 32])]);

        apply_candidates(&tx, 100, None, &[update], &mut live_names, &tx_law, &fvk).unwrap();

        assert_eq!(count(&tx, "SELECT COUNT(*) FROM names"), 0);
        assert_eq!(count(&tx, "SELECT COUNT(*) FROM name_events"), 0);
        assert_eq!(count(&tx, "SELECT COUNT(*) FROM implicit_releases"), 1);
        assert!(!live_names.contains(&TipNf::from_bytes(&[0x5a; 32])));
    }

    /// The mint spends the live tip in one action and writes the release
    /// note in another. `accept_release` still ends the binding: the tip
    /// nullifier is among the transaction's spends, and the predecessor matches.
    #[test]
    fn release_binds_when_the_tip_spend_is_a_different_action() {
        let conn = db();
        let tx = conn.unchecked_transaction().unwrap();
        let fvk = fvk();

        conn.execute(
            "INSERT INTO names (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, memo)
             VALUES ('jesuschrist', 90, 'claim', 'u', 'none', x'00', x'0202020202020202020202020202020202020202020202020202020202020202', x'03', x'04', ?1, x'06', 0, 0, x'07')",
            params![vec![0x5a_u8; 32]],
        )
        .unwrap();

        let memos = vec![memo_for("release", "jesuschrist", &[0x02; 32])];
        let candidates = vec![candidate(
            &memos,
            0,
            0,
            0,
            [3; 32],
            "release",
            "jesuschrist",
            [0x02; 32],
        )];
        let seeded = seeded_lineage(&[1]);
        let tx_law = vec![tx_law_for(
            [3; 32],
            seeded,
            vec![],
            vec![
                Retirement {
                    nf: AnchorNf::from_bytes(&[0x5a; 32]),
                },
                Retirement { nf: nf(9) },
            ],
            true,
        )];
        let mut live_names: HashSet<TipNf> = HashSet::from([TipNf::from_bytes(&[0x5a; 32])]);

        apply_candidates(&tx, 100, None, &candidates, &mut live_names, &tx_law, &fvk).unwrap();

        assert_eq!(count(&tx, "SELECT COUNT(*) FROM names"), 0);
        assert_eq!(count(&tx, "SELECT COUNT(*) FROM implicit_releases"), 0);
        assert_eq!(
            tx.query_row(
                "SELECT action FROM name_events WHERE name = 'jesuschrist' ORDER BY height DESC LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap(),
            "release"
        );
    }

    /// A transaction with two candidates takes follow_spends: nothing is
    /// admitted, and spent live names end their bindings.
    #[test]
    fn multi_candidate_tx_takes_follow_spends() {
        let conn = db();
        let tx = conn.unchecked_transaction().unwrap();
        let fvk = fvk();

        conn.execute(
            "INSERT INTO names (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, confirmed_mtp, memo)
             VALUES ('z', 90, 'claim', 'u', 'none', x'00', x'0202020202020202020202020202020202020202020202020202020202020202', x'03', x'04', ?1, x'06', 0, 0, 0, x'07')",
            params![vec![0x5a_u8; 32]],
        )
        .unwrap();

        let memos = vec![
            memo_for("claim", "a", &[0u8; 32]),
            memo_for("claim", "b", &[0u8; 32]),
        ];
        let candidates = vec![
            candidate(&memos, 0, 0, 0, [1; 32], "claim", "a", [0u8; 32]),
            candidate(&memos, 1, 0, 1, [1; 32], "claim", "b", [0u8; 32]),
        ];
        let seeded = seeded_lineage(&[1]);
        let tx_law = vec![tx_law_for(
            [1; 32],
            seeded.clone(),
            vec![],
            vec![Retirement {
                nf: AnchorNf::from_bytes(&[0x5a; 32]),
            }],
            false, // two candidates: follow_spends
        )];
        let mut live_names: HashSet<TipNf> = HashSet::from([TipNf::from_bytes(&[0x5a; 32])]);
        let mut lineage = seeded;
        lineage.step_tx(100, &tx_law[0].facts);

        apply_candidates(&tx, 100, None, &candidates, &mut live_names, &tx_law, &fvk).unwrap();

        assert_eq!(count(&tx, "SELECT COUNT(*) FROM names"), 0);
        assert_eq!(count(&tx, "SELECT COUNT(*) FROM implicit_releases"), 1);
        assert_eq!(count(&tx, "SELECT COUNT(*) FROM name_events"), 0);
    }

    /// A candidate-free transaction that spends a live tip takes
    /// follow_spends: the binding ends even though no candidate existed to
    /// reject. The mint applies this to every transaction that is not
    /// exactly-one-candidate.
    #[test]
    fn candidate_free_tx_spending_a_live_tip_ends_the_binding() {
        let conn = db();
        let tx = conn.unchecked_transaction().unwrap();
        let fvk = fvk();

        conn.execute(
            "INSERT INTO names (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, confirmed_mtp, memo)
             VALUES ('z', 90, 'claim', 'u', 'none', x'00', x'0202020202020202020202020202020202020202020202020202020202020202', x'03', x'04', ?1, x'06', 0, 0, 0, x'07')",
            params![vec![0x5a_u8; 32]],
        )
        .unwrap();

        // No candidates at all: a spend-only transaction.
        let seeded = seeded_lineage(&[1]);
        let tx_law = vec![tx_law_for(
            [7; 32],
            seeded.clone(),
            vec![],
            vec![Retirement {
                nf: AnchorNf::from_bytes(&[0x5a; 32]),
            }],
            false,
        )];
        let mut live_names: HashSet<TipNf> = HashSet::from([TipNf::from_bytes(&[0x5a; 32])]);
        let mut lineage = seeded;
        lineage.step_tx(100, &tx_law[0].facts);

        let candidates: Vec<Candidate> = vec![];
        apply_candidates(&tx, 100, None, &candidates, &mut live_names, &tx_law, &fvk).unwrap();

        assert_eq!(count(&tx, "SELECT COUNT(*) FROM names"), 0);
        assert_eq!(count(&tx, "SELECT COUNT(*) FROM implicit_releases"), 1);
    }

    /// H002, fixed: a clock-due update is a release, not a renewal. The
    /// mint frees the name internally on confirming it (zns-mint
    /// accept_update -> is_release_due -> release_predecessor); the
    /// resolver now ends the binding the same way, so the mint's re-sell
    /// lands here too.
    #[test]
    fn clock_due_update_ends_the_binding_and_the_re_claim_lands() {
        let conn = db();
        let tx = conn.unchecked_transaction().unwrap();
        let fvk = fvk();

        // z confirmed at mtp 1_000_000_000; the update confirms at
        // 1_000_000_000 + LIVENESS_INTERVAL + 1 — past both a "none"
        // term's liveness clock.
        conn.execute(
            "INSERT INTO names (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, confirmed_mtp, memo)
             VALUES ('z', 90, 'claim', 'u', 'none', x'00', x'0202020202020202020202020202020202020202020202020202020202020202', x'03', x'04', ?1, x'06', 0, 0, 1000000000, x'07')",
            params![vec![0x5a_u8; 32]],
        )
        .unwrap();

        let memos = vec![memo_for("update", "z", &[0x02; 32])];
        let update = candidate(&memos, 0, 0, 0, [1; 32], "update", "z", [0x02; 32]);

        let seeded = seeded_lineage(&[1]);
        let tx_law = vec![tx_law_for(
            [1; 32],
            seeded.clone(),
            vec![],
            vec![Retirement {
                nf: AnchorNf::from_bytes(&[0x5a; 32]),
            }],
            true,
        )];
        let mut live_names: HashSet<TipNf> = HashSet::from([TipNf::from_bytes(&[0x5a; 32])]);

        let stale_mtp = 1_000_000_000 + LIVENESS_INTERVAL + 1;
        let candidates = vec![update];
        apply_candidates(
            &tx,
            100,
            Some(stale_mtp),
            &candidates,
            &mut live_names,
            &tx_law,
            &fvk,
        )
        .unwrap();

        assert_eq!(count(&tx, "SELECT COUNT(*) FROM names"), 0, "binding ended");
        assert_eq!(count(&tx, "SELECT COUNT(*) FROM implicit_releases"), 1);

        // The victim's re-claim — anchor-backed exactly as the mint
        // assembles after re-selling the freed name — now lands.
        let memos2 = vec![memo_for("claim", "z", &[0u8; 32])];
        let victim = candidate(&memos2, 0, 0, 0, [2; 32], "claim", "z", [0u8; 32]);
        let tx_law2 = vec![tx_law_for(
            [2; 32],
            seeded.clone(),
            vec![Adoption { nf: nf(200) }],
            vec![Retirement { nf: nf(1) }],
            true,
        )];
        let candidates2 = vec![victim];
        apply_candidates(
            &tx,
            101,
            Some(stale_mtp),
            &candidates2,
            &mut live_names,
            &tx_law2,
            &fvk,
        )
        .unwrap();

        let action: String = tx
            .query_row("SELECT action FROM names WHERE name = 'z'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(action, "claim", "the victim's paid claim resolves");
    }

    /// The clocks must not over-fire: an update well within both clocks
    /// renews normally.
    #[test]
    fn clock_fresh_update_renews() {
        let conn = db();
        let tx = conn.unchecked_transaction().unwrap();
        let fvk = fvk();

        conn.execute(
            "INSERT INTO names (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, confirmed_mtp, memo)
             VALUES ('z', 90, 'claim', 'u', 'none', x'00', x'0202020202020202020202020202020202020202020202020202020202020202', x'03', x'04', ?1, x'06', 0, 0, 1000000000, x'07')",
            params![vec![0x5a_u8; 32]],
        )
        .unwrap();

        let memos = vec![memo_for("update", "z", &[0x02; 32])];
        let update = candidate(&memos, 0, 0, 0, [1; 32], "update", "z", [0x02; 32]);
        let seeded = seeded_lineage(&[1]);
        let tx_law = vec![tx_law_for(
            [1; 32],
            seeded,
            vec![],
            vec![Retirement {
                nf: AnchorNf::from_bytes(&[0x5a; 32]),
            }],
            true,
        )];
        let mut live_names: HashSet<TipNf> = HashSet::from([TipNf::from_bytes(&[0x5a; 32])]);

        let candidates = vec![update];
        apply_candidates(
            &tx,
            100,
            Some(1_000_000_500),
            &candidates,
            &mut live_names,
            &tx_law,
            &fvk,
        )
        .unwrap();

        let action: String = tx
            .query_row("SELECT action FROM names WHERE name = 'z'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(action, "update");
        assert_eq!(count(&tx, "SELECT COUNT(*) FROM implicit_releases"), 0);
    }

    /// The term clock is the predecessor's, not the update memo's. A lapsed
    /// term ends the binding even when the update itself carries no expiry.
    #[test]
    fn clock_due_term_ends_the_binding() {
        let conn = db();
        let tx = conn.unchecked_transaction().unwrap();
        let fvk = fvk();

        conn.execute(
            "INSERT INTO names (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, confirmed_mtp, memo)
             VALUES ('z', 90, 'claim', 'u', '1000', x'00', x'0202020202020202020202020202020202020202020202020202020202020202', x'03', x'04', ?1, x'06', 0, 0, 5000000, x'07')",
            params![vec![0x5a_u8; 32]],
        )
        .unwrap();

        let memos = vec![memo_for("update", "z", &[0x02; 32])];
        let update = candidate(&memos, 0, 0, 0, [1; 32], "update", "z", [0x02; 32]);
        let seeded = seeded_lineage(&[1]);
        let tx_law = vec![tx_law_for(
            [1; 32],
            seeded,
            vec![],
            vec![Retirement {
                nf: AnchorNf::from_bytes(&[0x5a; 32]),
            }],
            true,
        )];
        let mut live_names: HashSet<TipNf> = HashSet::from([TipNf::from_bytes(&[0x5a; 32])]);

        apply_candidates(
            &tx,
            100,
            Some(2_000),
            &[update],
            &mut live_names,
            &tx_law,
            &fvk,
        )
        .unwrap();

        assert_eq!(count(&tx, "SELECT COUNT(*) FROM names"), 0);
        assert_eq!(count(&tx, "SELECT COUNT(*) FROM implicit_releases"), 1);
    }

    /// A batch with one received zero-value note is not the keygen
    /// transaction, so that note is not stored. A payment and a note this
    /// account sent are not stored either. The block time is still recorded.
    #[test]
    fn a_batch_skips_a_note_outside_the_keygen_transaction_and_a_payment() {
        use orchard::note::Nullifier;
        use seer_sync::Cursor;
        use zcash_primitives::block::BlockHash;
        use zcash_primitives::transaction::TxId;
        use zcash_protocol::consensus::BlockHeight;

        use crate::registry::batch::BatchOutput;

        let conn = db();
        conn.execute(
            "INSERT INTO registry_account (id, ufvk, network, birthday) VALUES (0, 'ufvk', 'test', 1)",
            [],
        )
        .unwrap();

        let nf_bytes = [9u8; 32];
        let nullifier = Option::from(Nullifier::from_bytes(&nf_bytes)).expect("nullifier");

        let tx = BatchTx {
            txid: TxId::from_bytes([1; 32]),
            height: BlockHeight::from_u32(10),
            tx_index: 0,
            ironwood_outputs: vec![
                BatchOutput {
                    index: 0,
                    value: 0,
                    nf: Some(nullifier),
                    is_sent: false,
                },
                BatchOutput {
                    index: 1,
                    value: 1,
                    nf: Some(nullifier),
                    is_sent: false,
                },
                BatchOutput {
                    index: 2,
                    value: 0,
                    nf: Some(nullifier),
                    is_sent: true,
                },
            ],
            ironwood_spends: vec![],
            relaxed_ironwood_outputs: vec![],
        };
        apply_batch(
            &conn,
            Cursor {
                height: BlockHeight::from_u32(10),
                hash: BlockHash([2; 32]),
            },
            &[tx],
            &[(10, 1_000)],
            &fvk(),
            Lineage::new(),
        )
        .unwrap();

        assert_eq!(count(&conn, "SELECT COUNT(*) FROM names"), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM anchor_facts"), 0);
        let time: i64 = conn
            .query_row(
                "SELECT time FROM block_times WHERE height = 10",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(time, 1_000);
    }

    fn seeded_lineage(anchors: &[u8]) -> Lineage {
        let mut lineage = Lineage::new();
        for seed in anchors {
            lineage.adopt(0, nf(*seed));
        }
        lineage
    }
}
