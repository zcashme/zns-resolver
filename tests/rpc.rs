//! End-to-end RPC wire compatibility.
//!
//! zcashname-sdk sends *named* (object) JSON-RPC params — `{"query": …}`,
//! `{"name": …, "limit": …}` — so this test seeds a real on-disk index through
//! `apply_notes` (genuine crypto, same path as the scanner) and queries the
//! served API exactly as the SDK does, asserting the documented response
//! shapes: uppercase action names, newest-first events, bare-array reverse
//! lookup.

use group::ff::PrimeField;
use jsonrpsee::core::client::ClientT;
use jsonrpsee::core::params::ObjectParams;
use jsonrpsee::http_client::HttpClientBuilder;
use orchard::keys::{FullViewingKey, Scope, SpendingKey};
use orchard::note::{RandomSeed, Rho};
use orchard::value::NoteValue;
use orchard::Note;
use pasta_curves::pallas;
use seer_sync::BlockHeight;
use serde_json::Value;
use zcash_protocol::memo::MemoBytes;

use zns_resolver::http::{serve, RpcContext};
use zns_resolver::index::{Cursor, NameNote, SqliteIndex};
use zns_resolver::ZERO_PREV_RCM;
use zns_verify::{note_commitment_cmx, zns_psi_rcm, Action};

/// A value-0 Orchard note shaped like a ZNS Name Note (see tests/binding.rs).
fn name_note() -> Note {
    let sk = SpendingKey::from_bytes([1u8; 32]).into_option().expect("valid spending key");
    let fvk = FullViewingKey::from(&sk);
    let recipient = fvk.address_at(0u32, Scope::External);
    let rho = Rho::from_bytes(&[1u8; 32]).into_option().expect("valid rho");
    let rseed = RandomSeed::from_bytes([2u8; 32], &rho).into_option().expect("valid rseed");
    Note::from_parts(recipient, NoteValue::from_raw(0), rho, rseed)
        .into_option()
        .expect("valid note")
}

fn genuine_cmx(note: &Note, action: Action, name: &str, ua: &str, prev_rcm: &[u8; 32]) -> [u8; 32] {
    let (psi, rcm) = zns_psi_rcm(action.as_bytes(), name.as_bytes(), ua.as_bytes(), prev_rcm);
    let (g_d, pk_d) = note.recipient().zns_commitment_keys();
    let rho = pallas::Base::from_repr(note.rho().to_bytes()).expect("rho is a canonical base");
    note_commitment_cmx(g_d, pk_d, note.value().inner(), rho, psi, rcm)
        .expect("commitment is not the identity")
        .to_repr()
}

/// Seed a CLAIM@100 + UPDATE@105 for "alice" through the real verify path.
fn seed_index(db: &std::path::Path) {
    let idx = SqliteIndex::open(db).unwrap();
    let note = name_note();
    let claim_cmx = genuine_cmx(&note, Action::Claim, "alice", "u1old", &ZERO_PREV_RCM);
    let (_, claim_rcm) = zns_psi_rcm(Action::Claim.as_bytes(), b"alice", b"u1old", &ZERO_PREV_RCM);
    let update_cmx = genuine_cmx(&note, Action::Update, "alice", "u1new", &claim_rcm.to_repr());

    let memo = |s: &str| MemoBytes::from_bytes(s.as_bytes()).unwrap();
    let batch = [
        NameNote {
            note,
            cmx: claim_cmx,
            memo: memo("ZNS:claim:alice:u1old"),
            txid: [0xAA; 32],
            height: 100,
        },
        NameNote {
            note,
            cmx: update_cmx,
            memo: memo("ZNS:update:alice:u1new"),
            txid: [0xBB; 32],
            height: 105,
        },
    ];
    idx.apply_notes(Cursor { height: BlockHeight::from_u32(105), hash: None }, &batch).unwrap();
}

#[tokio::test]
async fn rpc_serves_sdk_shaped_requests() {
    let db = std::env::temp_dir().join(format!("zns-resolver-rpc-test-{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db);
    seed_index(&db);

    // An OS-assigned free port; the tiny re-bind race is fine for a test.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let addr = format!("127.0.0.1:{port}");
    let ctx = RpcContext { db: db.clone(), uivk: "uivk1test".into() };
    tokio::spawn(async move { serve(&addr, ctx).await });

    let client = HttpClientBuilder::default().build(format!("http://127.0.0.1:{port}")).unwrap();

    // `status` — wait for the server to come up.
    let mut status: Option<Value> = None;
    for _ in 0..50 {
        match client.request("status", ObjectParams::new()).await {
            Ok(v) => {
                status = Some(v);
                break;
            }
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
        }
    }
    let status = status.expect("RPC server did not come up");
    assert_eq!(status["synced_height"], 105);
    assert_eq!(status["registered"], 1);
    assert_eq!(status["uivk"], "uivk1test");

    // `resolve` by exact name, named params, `with_proof` omitted (SDK-style).
    let mut p = ObjectParams::new();
    p.insert("query", "alice").unwrap();
    p.insert("limit", 50u64).unwrap();
    p.insert("offset", 0u64).unwrap();
    let reg: Value = client.request("resolve", p).await.unwrap();
    assert_eq!(reg["name"], "alice");
    assert_eq!(reg["address"], "u1new");
    assert_eq!(reg["last_action"], "UPDATE");

    // `resolve` by address — a bare array of that UA's registrations.
    let mut p = ObjectParams::new();
    p.insert("query", "u1new").unwrap();
    let by_ua: Value = client.request("resolve", p).await.unwrap();
    assert_eq!(by_ua[0]["name"], "alice");

    // `events` filtered by name — newest first, total pre-pagination.
    let mut p = ObjectParams::new();
    p.insert("name", "alice").unwrap();
    p.insert("limit", 50u64).unwrap();
    let ev: Value = client.request("events", p).await.unwrap();
    assert_eq!(ev["total"], 2);
    assert_eq!(ev["events"][0]["action"], "UPDATE");
    assert_eq!(ev["events"][0]["ua"], "u1new");
    assert_eq!(ev["events"][1]["action"], "CLAIM");
    assert_eq!(ev["events"][1]["height"], 100);

    // An action verb this resolver can never log matches zero events.
    let mut p = ObjectParams::new();
    p.insert("action", "BUY").unwrap();
    let ev: Value = client.request("events", p).await.unwrap();
    assert_eq!(ev["total"], 0);

    let _ = std::fs::remove_file(&db);
}
