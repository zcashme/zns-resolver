//! ZcashName (ZNS) resolver.
//!
//! Thin consumer of [`seer-sync`](https://crates.io/crates/seer-sync) that
//! plugs a ZNS-specific [`ScanCallback`](seer_sync::scan::ScanCallback)
//! into the generic view-key sync engine, and persists the results in a
//! narrow SQLite index keyed by name.
//!
//! Modules:
//!  - [`verify`] — ZNS binding verification + memo parsing.
//!  - [`index`]  — SQLite name index and the `seer-sync` `Account` impl that
//!    drives it (verify-on-`apply`, reorg via `rewind`). The Orchard
//!    note-commitment tree (for inclusion witnesses) is maintained here too, via
//!    seer-sync's `commitment_tree` store over the same connection.
//!  - [`http`]   — jsonrpsee JSON-RPC API (`resolve`, `status`).

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

pub mod http;
pub mod index;
pub mod verify;

pub use zns_verify::{Action, ZERO_PREV_RCM};

/// NU5 activation height on testnet — earliest possible Orchard note.
pub const TESTNET_NU5_HEIGHT: u32 = 1_842_420;

/// NU5 activation height on mainnet — earliest possible Orchard note.
pub const MAINNET_NU5_HEIGHT: u32 = 1_687_104;
