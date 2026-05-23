//! ZcashName (ZNS) resolver.
//!
//! Thin consumer of [`seer-sync`](https://crates.io/crates/seer-sync) that
//! plugs a ZNS-specific [`ScanCallback`](seer_sync::scan::ScanCallback)
//! into the generic view-key sync engine, and persists the results in a
//! narrow SQLite index keyed by name.
//!
//! Modules:
//!  - [`verify`] — ZNS binding verification + chain advancement rules.
//!  - [`callback`] — the `ScanCallback` impl that runs verification per decrypt.
//!  - [`index`]  — narrow 3-table SQLite index with per-block transactions.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

pub mod callback;
pub mod index;
pub mod verify;

pub use zns_verify::{Action, ZERO_PREV_RCM};

/// NU5 activation height on testnet — earliest possible Orchard note.
pub const TESTNET_NU5_HEIGHT: u32 = 1_842_420;

/// NU5 activation height on mainnet — earliest possible Orchard note.
pub const MAINNET_NU5_HEIGHT: u32 = 1_687_104;
