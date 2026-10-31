# zcashname-sdk

Rust SDK for the Zcash Name System resolver and mint request memos, with the
features of Dart SDK 0.8.0. Requires Rust 1.88 or newer.

Version **0.2.0 is unreleased**. Use a local dependency from this repository's
root while developing:

```toml
[dependencies]
zcashname-sdk = { path = "sdk/rust" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

## Resolver client

```rust,no_run
use zcashname_sdk::{Network, ResolverClient};

#[tokio::main]
async fn main() -> Result<(), zcashname_sdk::Error> {
    // ResolverClient::default() uses testnet. Select mainnet explicitly.
    let resolver = ResolverClient::for_network(Network::Mainnet);
    resolver.verify().await?;

    if let Some(record) = resolver.resolve_name(" Alice ").await? {
        println!("alice resolves to {}", record.address);
    }
    let page = resolver.list_names(Some(50), Some(0)).await?;
    println!("{} total names, {} on this page", page.total, page.items.len());

    let status = resolver.status().await?;
    println!("synced: {:?} at {}", status.synced, status.synced_height);
    Ok(())
}
```

The client exposes `resolve`, `resolve_name`, `is_available`, `list_names`,
`reverse_lookup`, `resolve_address`, `list_all_registrations`, `events`,
`status`, and `verify`. The two list shorthands return the items of one page;
they do not traverse every page. `EventsFilter::default()` selects no filters.
Name lookups and event name filters trim whitespace and lowercase ASCII only;
they do not remove suffixes. Invalid lookup names return `None`, or `false`
for availability, without contacting the resolver.

`Network::config()` exposes immutable resolver URLs, the registry viewing key
and mint treasury address. `verify()` compares the reported key with this
network's built-in key. A mismatch can mean the endpoint is wrong or the SDK
needs updating after a registry rotation; do not trust that endpoint's data.

Mainnet tries its primary and fallback endpoints in order. Each request has a
10-second timeout, including the response body. Transport errors, HTTP 5xx,
malformed responses and mismatched response IDs try the next endpoint. HTTP
4xx and JSON-RPC errors are returned immediately. The last failure is returned
when all endpoints fail.

`ResolverClient::new(url)` uses a single endpoint and testnet identity. For
an explicit network, timeout, endpoint override or shared HTTP client:

```rust,no_run
use std::time::Duration;
use zcashname_sdk::{Network, ResolverClient};

let resolver = ResolverClient::with_options(
    Network::Mainnet,
    Some(vec!["https://my-resolver.example/rpc".into()]),
    reqwest::Client::new(),
    Duration::from_secs(5),
)?;
# Ok::<(), zcashname_sdk::Error>(())
```

An endpoint override replaces the entire preset list. Pass `None` to retain
the network's endpoints. Dropping the client releases its HTTP client handle;
no explicit close is needed.

Records follow the resolver's JSON fields (`last_action`, `since_height`,
`action_index`). `expires_at` is `"none"` or a decimal Unix timestamp. `Status`
includes `synced_height`, optional `synced`, `viewing_key`, and optional
`treasury_address`.

## Mint memos and payment URIs

```rust
use zcashname_sdk::{build_claim_memo, build_zip321_uri, Network, Term};

fn claim_payment(ua: &str, amount_zec: &str) -> Result<String, zcashname_sdk::Error> {
    let memo = build_claim_memo("alice", ua, Term::Forever, None)?;
    build_zip321_uri(
        Network::Mainnet.config().treasury_address,
        Some(amount_zec),
        Some(&memo),
    )
}
```

- `build_claim_memo(name, ua, term, access_code)` supports optional six-digit
  access codes for protected names.
- `build_update_memo(name, ua, term)` preserves the expiry when `term` is `None`.
- `build_release_memo(name, ua)` builds a release request.
- `build_otp_memo(&OtpMemo)` and `parse_otp_memo(text)` handle update/release
  challenges, including trailing NUL memo padding.
- `Term::Years(1..=99)` and `Term::Forever` select the registration term.

Memo builders require canonical names and valid unified address checksums.
`is_valid_name`, `normalize_name`, and `is_valid_unified_address` are public.
Address validation checks allowed lengths and Bech32m; it does not decode
receivers or prove that an Orchard receiver is present.

`build_zip321_uri` accepts an optional decimal ZEC amount with at most eight
fractional digits, bounded by 21,000,000 ZEC. It encodes a UTF-8 memo as unpadded
base64url, enforces the 512-byte limit and rejects memos sent to transparent
addresses. `parse_zip321_uri`, `to_base64_url`, and `decode_base64_url` provide
the corresponding parsing and encoding helpers.

## Display pricing

`annual_usd(name)` returns USD tiers of 10,000 / 2,500 / 800 / 400 / 100 for
lengths 1–5, and 20 for longer names. Empty input uses the first tier.
`forever_usd(name)` charges three annual prices. `quote_zats(usd, zats_per_usd)`
rounds up at a supplied oracle rate and returns `Result<u64, Error>`, rejecting
negative, non-finite or overflowing values. These are display estimates;
the mint computes the authoritative payment amount.

## Dependencies and validation

Runtime dependencies are `reqwest` (JSON and Rustls TLS; default features
disabled), `serde`, `serde_json`, `thiserror`, and `base64`. `tokio` is a
development dependency for async HTTP tests; applications provide their own
compatible async runtime.

```sh
cargo fmt --manifest-path sdk/rust/Cargo.toml --check
cargo clippy --manifest-path sdk/rust/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path sdk/rust/Cargo.toml
```
