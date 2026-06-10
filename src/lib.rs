//! ZcashName (ZNS) resolver.
//!
//! Drives its own chain-observer loop over [`seer-sync`](https://crates.io/crates/seer-sync)'s
//! block stream — relaxed-decrypting Orchard actions with the ZNS kernel,
//! verifying each Name Note's binding, and folding the results into a narrow
//! SQLite index keyed by name. seer-sync is used as a toolkit (block stream,
//! action parsing), never via its `run`/`Account` engine, so the relaxed
//! decrypt stays in `zns-verify` and seer-sync stays ZNS-blind.
//!
//! Modules:
//!  - [`verify`]  — ZNS binding verification (memo grammar lives in the
//!    kernel, `zns_verify::memo`).
//!  - [`observe`] — the chain-observer scan loop: stream → relaxed decrypt →
//!    memo recovery → verify → index, with reorg rollback.
//!  - [`index`]   — SQLite name index (verify-on-apply, reorg via `rewind`).
//!  - [`proof`]   — proof-material acquisition: validator RPC + Merkle
//!    branch construction (`PROOFS.md`).
//!  - [`http`]    — jsonrpsee JSON-RPC API (`resolve`, `chain`, `status`).

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

pub mod http;
pub mod index;
pub mod net;
pub mod observe;
pub mod proof;
pub mod verify;

pub use zns_verify::{Action, ZERO_PREV_RCM};

/// NU5 activation height on testnet — earliest possible Orchard note.
pub const TESTNET_NU5_HEIGHT: u32 = 1_842_420;

/// NU5 activation height on mainnet — earliest possible Orchard note.
pub const MAINNET_NU5_HEIGHT: u32 = 1_687_104;
