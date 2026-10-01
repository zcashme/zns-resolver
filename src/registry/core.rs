//! Transactional core of the registry.

use std::collections::{HashMap, HashSet};

use orchard::keys::{FullViewingKey, Scope};
use orchard::note::Note;
use orchard::note::NoteCommitTrapdoor;
use orchard::note::Nullifier;
use rusqlite::{self as rusqlite, params, Connection, OptionalExtension, Row, Transaction};
use seer_sync::sync::scan::WalletTx;
use seer_sync::{Cursor, Nullifiers, Resume};
use zcash_address::unified::Encoding as _;
use zcash_primitives::block::BlockHash;
use zcash_protocol::consensus::{BlockHeight, Parameters as _};
use zns_verify::{pallas, Action, Memo, NameNote, PrimeField, Tip};

use super::anchor_lineage::{Adoption, Lineage, Position, Retirement, TxAnchorFacts};
use super::nf::AnchorNf;
use super::notes;
use super::{Event, Registration};

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
    if let Some((stored_ufvk, stored_net, stored_birthday)) = registry_config(conn)? {
        if stored_ufvk != ufvk {
            tracing::warn!(
                stored = %stored_ufvk,
                "registry_account ufvk already set; not changing"
            );
        }
        if stored_net != network {
            tracing::warn!(
                stored = %stored_net,
                "registry_account network already set; not changing"
            );
        }
        if stored_birthday != birthday as i64 {
            tracing::warn!(
                stored = stored_birthday,
                "registry_account birthday already set; not changing"
            );
        }
        return Ok(());
    }

    conn.execute(
        "INSERT INTO registry_account (id, ufvk, network, birthday) VALUES (0, ?1, ?2, ?3)",
        params![ufvk, network, birthday as i64],
    )?;
    Ok(())
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
/// state, never partial. The connection lock makes this the sole mutator;
/// tip reads and writes share the transaction.
pub(crate) fn apply_batch(
    conn: &Connection,
    scanned: Cursor,
    transactions: &[WalletTx],
    fvk: &FullViewingKey,
) -> rusqlite::Result<()> {
    let db_tx = conn.unchecked_transaction()?;

    // The lineage as of the batch start, folded from the stored facts; the
    // live name nullifiers feed the claim law's no-name-spend condition.
    let mut lineage = load_lineage(&db_tx)?;
    let mut live_names: HashSet<[u8; 32]> = live_name_nullifiers(&db_tx)?;

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
            for output in &tx.relaxed_ironwood_outputs {
                let (action_index, cand_note, _action_nullifier, memo, _) = output;

                let Some(memo) = memo else {
                    continue;
                };
                let Ok(zns_memo) = Memo::from_bytes(memo) else {
                    continue;
                };
                // Gate: protocol parse. The kernel's structural rules are the
                // authority — invalid statements never become candidates.
                let Ok(note) = NameNote::parse(&zns_memo) else {
                    continue;
                };
                // Gate: binding — the transition, hashed under the ZNS
                // binding, must reproduce the published cmx.
                let Some((psi, rcm)) =
                    notes::verify_commitment(&note, cand_note.note(), &cand_note.cmx().to_bytes())
                else {
                    continue;
                };
                // Gate: shape — zero value, registry recipient.
                if cand_note.note().value().inner() != 0
                    || cand_note.note().recipient() != registry_recipient
                {
                    continue;
                }
                // Gate: the bound UA must decode as a Unified Address on this
                // network (restores 304193b, lost in the #27 refactor).
                let ua = note.ua().as_str();
                let Some((ua_network, _)) = zcash_address::unified::Address::decode(ua).ok() else {
                    continue;
                };
                if ua_network != crate::NETWORK.network_type() {
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
        let candidates: Vec<Candidate> = memos
            .iter()
            .zip(&sources)
            .map(|(memo, src)| Candidate {
                action_index: src.action_index,
                txid: src.txid,
                memo: src.memo,
                note: NameNote::parse(memo).expect("arena memo parsed at triage"),
                cand_cmx: src.cand_cmx,
                note_orchard: src.cand_note,
                psi: src.psi,
                rcm: src.rcm,
            })
            .collect();

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
            for output in &tx_data.ironwood_outputs {
                if !output.is_sent {
                    if let Some(nf) = output.nf {
                        let nf = AnchorNf::from_scan(nf);
                        db_tx.execute(
                            "INSERT OR IGNORE INTO anchor_facts (nullifier, value, height, tx_index, action_index, name_note_candidates, spent_height)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
                            params![
                                nf.as_bytes().as_slice(),
                                output.note.value().inner() as i64,
                                block_height as i64,
                                tx_index as i64,
                                output.index as i64,
                                candidate_count as i64,
                            ],
                        )?;
                    }
                }
            }
            for spend in &tx_data.ironwood_spends {
                let nf = AnchorNf::from_scan(spend.nf);
                db_tx.execute(
                    "UPDATE anchor_facts SET spent_height = ?1, spent_tx_index = ?2, spent_action_index = ?3
                     WHERE nullifier = ?4 AND spent_height IS NULL",
                    params![
                        block_height as i64,
                        tx_index as i64,
                        spend.index as i64,
                        nf.as_bytes().as_slice()
                    ],
                )?;
            }
        }

        // Per-transaction anchor facts and pre-transaction lineage
        // snapshots, in canonical order: a candidate is judged against the
        // lineage its own transaction was judged against.
        let mut txs: Vec<&WalletTx> = block.iter().collect();
        txs.sort_by_key(|tx| tx.tx_index);
        let mut tx_law: Vec<TxLaw> = Vec::new();
        for tx in txs {
            let txid = *tx.txid.as_ref();
            let tx_index = tx.tx_index;
            let facts = TxAnchorFacts {
                adoptions: tx
                    .ironwood_outputs
                    .iter()
                    .filter(|o| !o.is_sent && o.note.value().inner() == 0)
                    .filter_map(|o| o.nf.map(AnchorNf::from_scan))
                    .map(|nf| Adoption { nf })
                    .collect(),
                retirements: tx
                    .ironwood_spends
                    .iter()
                    .map(|s| Retirement {
                        nf: AnchorNf::from_scan(s.nf),
                    })
                    .collect(),
                has_single_name_note: candidate_counts.get(&txid).copied().unwrap_or(0) == 1,
            };
            let snapshot = lineage.clone();
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
        apply_candidates(&db_tx, height, &candidates, &mut live_names, &tx_law, fvk)?;
    }

    set_checkpoint_in_tx(&db_tx, &scanned)?;
    db_tx.commit()?;
    Ok(())
}

/// Folds the stored anchor facts into the lineage as of the last checkpoint.
/// The facts are replayed in canonical order; each zero-value received note
/// is an adoption at its own position, each spent zero-value note a
/// retirement at its spending position, and a transaction's candidate count
/// carries the accept-path marker.
fn load_lineage(conn: &Connection) -> rusqlite::Result<Lineage> {
    let mut stmt = conn.prepare(
        "SELECT nullifier, value, height, tx_index, action_index, name_note_candidates,
                spent_height, spent_tx_index, spent_action_index
         FROM anchor_facts",
    )?;
    let mut events: Vec<(Position, FactEvent)> = Vec::new();
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
    for row in rows {
        let (nf, value, height, tx_index, action_index, cands, spent_h, spent_tx, spent_a) = row?;
        if value != 0 {
            continue;
        }
        let nf_bytes: [u8; 32] = nf.try_into().map_err(|_| corrupt_record())?;
        let nf = AnchorNf::from_bytes(&nf_bytes);
        let adopt_at = Position {
            height: height as u32,
            tx_index: tx_index as u32,
            action_index: action_index as u32,
        };
        events.push((
            adopt_at,
            FactEvent::Adoption {
                nf,
                name_note_candidates: cands,
            },
        ));
        if let (Some(sh), Some(stx), Some(sa)) = (spent_h, spent_tx, spent_a) {
            events.push((
                Position {
                    height: sh as u32,
                    tx_index: stx as u32,
                    action_index: sa as u32,
                },
                FactEvent::Retirement(nf),
            ));
        }
    }
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
                } => {
                    facts.has_single_name_note = name_note_candidates == 1;
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
    Ok(lineage)
}

/// One replayed fact: an adoption carrying its transaction's candidate
/// count (the accept-path marker), or a retirement.
#[derive(Debug, Clone, Copy)]
enum FactEvent {
    Adoption {
        nf: AnchorNf,
        name_note_candidates: i64,
    },
    Retirement(AnchorNf),
}

/// The live name tips' nullifiers — the claim law's "spends no live name
/// note" set.
fn live_name_nullifiers(conn: &Connection) -> rusqlite::Result<HashSet<[u8; 32]>> {
    let mut stmt = conn.prepare("SELECT nullifier FROM names")?;
    let rows = stmt.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
    let mut set = HashSet::new();
    for bytes in rows {
        let bytes: Vec<u8> = bytes?;
        if let Ok(nf) = <[u8; 32]>::try_from(bytes) {
            set.insert(nf);
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
    candidates: &[Candidate],
    live_names: &mut HashSet<[u8; 32]>,
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
        let tip_nf = binding.as_ref().map(|b| b.1);
        let tip_consumed =
            tip_nf.is_some_and(|nf| tx_facts.retirements.iter().any(|r| r.nf.as_bytes() == &nf));
        let spent_a_live_name = tx_facts
            .retirements
            .iter()
            .any(|r| live_names.contains(r.nf.as_bytes()));

        match note.action() {
            Action::Claim => {
                // Gate: the mint's accept_claim. The anchor spend and the
                // name note are separate actions — fee inputs sit between
                // them — so the name note's own nullifier is not the anchor.
                // The transaction must spend exactly one live anchor, spend
                // no live name, and create one zero-value successor.
                let law_ok = snapshot.live_retirements(tx_facts) == 1
                    && !spent_a_live_name
                    && tx_facts.adoptions.len() == 1;
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
                    notes::check_chain_rule(binding.as_ref().map(|b| &b.0), note)
                else {
                    continue;
                };

                let Some(nullifier) = admit_nullifier(candidate, fvk) else {
                    continue;
                };
                record_admission(
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
                        nullifier: nullifier.as_slice(),
                        txid: candidate.txid.as_slice(),
                        tx_index: law.tx_index as i64,
                        action_index: candidate.action_index as i64,
                        memo: candidate.memo,
                    },
                )?;
                live_names.insert(nullifier);
            }
            Action::Update | Action::Release => {
                // Gate: predecessor — the transaction must consume this
                // name's live tip, only this name's, and no anchor. Anything
                // else is follow_spends: every spent live name ends its
                // binding.
                let spent_other_live = tx_facts.retirements.iter().any(|r| {
                    let nf = r.nf.as_bytes();
                    live_names.contains(nf) && tip_nf != Some(*nf)
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
                    notes::check_chain_rule(binding.as_ref().map(|b| &b.0), note)
                else {
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

                let Some(nullifier) = admit_nullifier(candidate, fvk) else {
                    continue;
                };
                record_admission(
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
                        nullifier: nullifier.as_slice(),
                        txid: candidate.txid.as_slice(),
                        tx_index: law.tx_index as i64,
                        action_index: candidate.action_index as i64,
                        memo: candidate.memo,
                    },
                )?;
                if let Some(b) = binding {
                    live_names.remove(&b.1);
                }
                if note.action() == Action::Update {
                    live_names.insert(nullifier);
                }
            }
        }
    }
    Ok(())
}

/// The admission nullifier of a verified candidate.
fn admit_nullifier(candidate: &Candidate, fvk: &FullViewingKey) -> Option<[u8; 32]> {
    candidate
        .note_orchard
        .zns_nullifier(
            fvk,
            NoteCommitTrapdoor::from_inner(candidate.rcm),
            candidate.psi,
        )
        .map(|n| n.to_bytes())
}

/// The mint's `mark_released`: every live name whose tip this transaction
/// spent ends its binding, recorded as an implicit release.
fn mark_released_spent(
    db_tx: &Transaction<'_>,
    live_names: &mut HashSet<[u8; 32]>,
    facts: &TxAnchorFacts,
    height: u32,
    txid: &[u8],
    tx_index: u32,
    action_index: usize,
) -> rusqlite::Result<()> {
    for retirement in &facts.retirements {
        let nf = retirement.nf.as_bytes();
        if !live_names.remove(nf) {
            continue;
        }
        let name: String = db_tx
            .query_row(
                "SELECT name FROM names WHERE nullifier = ?1",
                params![nf],
                |r| r.get(0),
            )
            .optional()?
            .expect("live-name set mirrors the names table");
        db_tx.execute("DELETE FROM names WHERE name = ?1", params![name])?;
        db_tx.execute(
            "INSERT OR IGNORE INTO implicit_releases (name, height, txid, tx_index, action_index, nullifier)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![name, height as i64, txid, tx_index as i64, action_index as i64, nf],
        )?;
    }
    Ok(())
}

/// The mint's `release_predecessor`: the tip was consumed without a valid
/// successor, so the binding ends — recorded as an implicit release.
#[allow(clippy::too_many_arguments)]
fn end_binding_implicitly(
    db_tx: &Transaction<'_>,
    live_names: &mut HashSet<[u8; 32]>,
    name: &str,
    height: u32,
    txid: &[u8],
    tx_index: u32,
    action_index: usize,
    consumed: &[u8; 32],
) -> rusqlite::Result<()> {
    live_names.remove(consumed);
    db_tx.execute("DELETE FROM names WHERE name = ?1", params![name])?;
    db_tx.execute(
        "INSERT OR IGNORE INTO implicit_releases (name, height, txid, tx_index, action_index, nullifier)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![name, height as i64, txid, tx_index as i64, action_index as i64, consumed],
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
    memo: &'a [u8],
}

fn record_admission(db_tx: &Transaction<'_>, row: &AdmissionRow<'_>) -> rusqlite::Result<()> {
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
        row.memo,
    ];
    db_tx.execute(
        "INSERT INTO name_events (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, memo)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        sql_params,
    )?;
    if row.action == "release" {
        db_tx.execute("DELETE FROM names WHERE name = ?1", params![row.name])?;
    } else {
        db_tx.execute(
            "INSERT INTO names (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, memo)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT (name) DO UPDATE SET
               height = excluded.height, action = excluded.action, ua = excluded.ua,
               expires_at = excluded.expires_at,
               prev_rcm = excluded.prev_rcm, rcm = excluded.rcm, psi = excluded.psi,
               cmx = excluded.cmx, nullifier = excluded.nullifier,
               txid = excluded.txid, tx_index = excluded.tx_index,
               action_index = excluded.action_index,
               memo = excluded.memo",
            sql_params,
        )?;
    }
    Ok(())
}

pub(crate) fn rewind(conn: &Connection, fork_height: u32) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;

    let mut stmt = tx.prepare(
        "SELECT name FROM name_events WHERE height > ?1
         UNION
         SELECT name FROM implicit_releases WHERE height > ?1",
    )?;
    let affected: Vec<String> = stmt
        .query_map(params![fork_height as i64], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    tx.execute(
        "DELETE FROM name_events WHERE height > ?1",
        params![fork_height as i64],
    )?;
    tx.execute(
        "DELETE FROM implicit_releases WHERE height > ?1",
        params![fork_height as i64],
    )?;
    tx.execute(
        "DELETE FROM anchor_facts WHERE height > ?1",
        params![fork_height as i64],
    )?;
    tx.execute(
        "UPDATE anchor_facts
         SET spent_height = NULL, spent_tx_index = NULL, spent_action_index = NULL
         WHERE spent_height > ?1",
        params![fork_height as i64],
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
        .filter_map(|nf| Option::from(Nullifier::from_bytes(nf.as_bytes())))
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
/// unspent watched ironwood notes plus every admitted name's nullifier.
pub(crate) fn ironwood_nullifiers(conn: &Connection) -> rusqlite::Result<Vec<AnchorNf>> {
    let mut statement = conn.prepare(
        "SELECT nullifier FROM anchor_facts WHERE spent_height IS NULL
         UNION
         SELECT nullifier FROM names",
    )?;
    let rows = statement.query_map([], |row| {
        let bytes: Vec<u8> = row.get(0)?;
        let nf: [u8; 32] = bytes.try_into().map_err(|_| corrupt_record())?;
        Ok(AnchorNf::from_bytes(&nf))
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

    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM name_events {WHERE}"),
        &p[..3],
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare(&format!(
        "SELECT rowid, name, action, ua, txid, height, action_index, memo FROM name_events {WHERE}
         ORDER BY height DESC, rowid DESC LIMIT ?4 OFFSET ?5"
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

/// Plain `SELECT` of a name's live state — the tip plus the stored nullifier.
/// Runs inside the batch transaction, so a candidate reads the admissions of
/// every earlier candidate in the same batch.
fn read_tip_offline(conn: &Connection, name: &str) -> rusqlite::Result<Option<(Tip, [u8; 32])>> {
    conn.query_row(
        "SELECT action, rcm, nullifier FROM names WHERE name = ?1",
        params![name],
        |row| {
            let action =
                Action::from_bytes(row.get::<_, String>(0)?.as_bytes()).ok_or(corrupt_record())?;
            let rcm: Vec<u8> = row.get(1)?;
            let rcm: [u8; 32] = rcm.try_into().map_err(|_| corrupt_record())?;
            let nullifier: Vec<u8> = row.get(2)?;
            let nullifier: [u8; 32] = nullifier.try_into().map_err(|_| corrupt_record())?;
            Ok((Tip { action, rcm }, nullifier))
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
            "SELECT action, memo, txid, height, tx_index, action_index, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier
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
            "INSERT INTO names (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, memo)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
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
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
        Some("registry record is corrupt".to_string()),
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
            "INSERT INTO name_events (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, memo)
             VALUES ('z', 90, 'claim', 'u', 'none', x'00', x'02', x'03', x'04', x'5a', x'06', 0, 0, ?1)",
            params![&memo[..]],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO names (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, memo)
             VALUES ('z', 90, 'claim', 'u', 'none', x'00', x'02', x'03', x'04', x'5a', x'06', 0, 0, ?1)",
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
            "INSERT INTO name_events (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, memo)
             VALUES ('z', 90, 'claim', 'u', 'none', x'00', x'02', x'03', x'04', x'5a', x'06', 0, 0, ?1)",
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
            "INSERT INTO name_events (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, memo)
             VALUES ('z', 100, 'claim', 'u', 'none', x'00', x'02', x'03', x'04', x'5b', x'08', 0, 0, x'09')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO names (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, memo)
             VALUES ('z', 100, 'claim', 'u', 'none', x'00', x'02', x'03', x'04', x'5b', x'08', 0, 0, x'09')",
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
        assert_eq!(version, 1);
        let facts: i64 = conn
            .query_row("SELECT COUNT(*) FROM anchor_facts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(facts, 0);
    }

    /// Schema version 1 is the current schema. Opening it again must not
    /// drop the index — the gate used to be `version < 2` while the schema
    /// wrote version 1, so every startup wiped the database.
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
        assert_eq!(version, 1);
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
        assert!(position.is_none()); // NULL hash: the next apply fixes it; a
                                     // restart meanwhile rescans from the birthday.
        let watched: u64 = conn
            .query_row("SELECT COUNT(*) FROM anchor_facts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(watched, 0);
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
        }
    }

    fn tx_law_for(
        txid: [u8; 32],
        snapshot: Lineage,
        adoptions: Vec<Adoption>,
        retirements: Vec<Retirement>,
        has_single_name_note: bool,
    ) -> TxLaw {
        TxLaw {
            txid,
            tx_index: 0,
            snapshot,
            facts: TxAnchorFacts {
                adoptions,
                retirements,
                has_single_name_note,
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
        let mut live_names: HashSet<[u8; 32]> = HashSet::new();
        let mut lineage = seeded;
        let _ = snapshot;

        // the fold already consumed the block's facts before admission
        lineage.step_tx(100, &tx_law[0].facts);

        apply_candidates(&tx, 100, &candidates, &mut live_names, &tx_law, &fvk).unwrap();

        assert_eq!(count(&tx, "SELECT COUNT(*) FROM names"), 1);
        assert_eq!(count(&tx, "SELECT COUNT(*) FROM name_events"), 1);
        assert_eq!(count(&tx, "SELECT COUNT(*) FROM implicit_releases"), 0);
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

        apply_candidates(&tx, 100, &candidates, &mut live_names, &tx_law, &fvk).unwrap();

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

        apply_candidates(&tx, 100, &candidates, &mut live_names, &tx_law, &fvk).unwrap();

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
            "INSERT INTO names (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, memo)
             VALUES ('z', 90, 'claim', 'u', 'none', x'00', x'0202020202020202020202020202020202020202020202020202020202020202', x'03', x'04', ?1, x'06', 0, 0, x'07')",
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
        let mut live_names: HashSet<[u8; 32]> = HashSet::from([[0x5a; 32]]);
        let mut lineage = seeded;
        lineage.step_tx(100, &tx_law[0].facts);

        apply_candidates(&tx, 100, &candidates, &mut live_names, &tx_law, &fvk).unwrap();

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
            "INSERT INTO names (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, memo)
             VALUES ('z', 90, 'claim', 'u', 'none', x'00', x'0202020202020202020202020202020202020202020202020202020202020202', x'03', x'04', ?1, x'06', 0, 0, x'07')",
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
                    nf: AnchorNf::from_bytes(&nullifier_u),
                }],
                true,
            ),
        ];

        let mut live_names: HashSet<[u8; 32]> = HashSet::from([[0x5a; 32]]);
        let mut lineage = seeded;
        lineage.step_tx(100, &tx_law[0].facts);
        lineage.step_tx(100, &tx_law[1].facts);

        let candidates = vec![update, release];
        apply_candidates(&tx, 100, &candidates, &mut live_names, &tx_law, &fvk).unwrap();

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
        assert_eq!(rel_nf, nullifier_u);
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
        let mut live_names: HashSet<[u8; 32]> = HashSet::from([[0x5a; 32]]);

        apply_candidates(&tx, 100, &candidates, &mut live_names, &tx_law, &fvk).unwrap();

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
            "INSERT INTO names (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, memo)
             VALUES ('z', 90, 'claim', 'u', 'none', x'00', x'0202020202020202020202020202020202020202020202020202020202020202', x'03', x'04', ?1, x'06', 0, 0, x'07')",
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
        let mut live_names: HashSet<[u8; 32]> = HashSet::from([[0x5a; 32]]);
        let mut lineage = seeded;
        lineage.step_tx(100, &tx_law[0].facts);

        apply_candidates(&tx, 100, &candidates, &mut live_names, &tx_law, &fvk).unwrap();

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
            "INSERT INTO names (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, tx_index, action_index, memo)
             VALUES ('z', 90, 'claim', 'u', 'none', x'00', x'0202020202020202020202020202020202020202020202020202020202020202', x'03', x'04', ?1, x'06', 0, 0, x'07')",
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
        let mut live_names: HashSet<[u8; 32]> = HashSet::from([[0x5a; 32]]);
        let mut lineage = seeded;
        lineage.step_tx(100, &tx_law[0].facts);

        let candidates: Vec<Candidate> = vec![];
        apply_candidates(&tx, 100, &candidates, &mut live_names, &tx_law, &fvk).unwrap();

        assert_eq!(count(&tx, "SELECT COUNT(*) FROM names"), 0);
        assert_eq!(count(&tx, "SELECT COUNT(*) FROM implicit_releases"), 1);
    }

    fn seeded_lineage(anchors: &[u8]) -> Lineage {
        let mut lineage = Lineage::new();
        for seed in anchors {
            lineage.step_tx(
                0,
                &TxAnchorFacts {
                    adoptions: vec![Adoption { nf: nf(*seed) }],
                    retirements: vec![],
                    has_single_name_note: false,
                },
            );
        }
        lineage
    }
}
