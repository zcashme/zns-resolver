//! The wallet's side of the bargain: resolve a name **and verify the answer**.
//!
//! Queries a resolver's `resolve(name, with_proof = true)`, then runs the
//! kernel walk (`zns_verify::proof::verify_chain`) over the returned links —
//! recomputing every binding against the on-chain commitments — and prints
//! the verified resolution plus the PoW anchors a real wallet would check
//! against its own header chain. This is the `PROOFS.md §3` flow end-to-end,
//! and the seed of the wallet SDK's resolution client.
//!
//! ```sh
//! cargo run --example verify_resolve -- \
//!     http://127.0.0.1:8080 alice <addr_reg UA> [--regtest|--mainnet]
//! ```

use anyhow::{anyhow, bail, Context, Result};
use jsonrpsee::core::client::ClientT;
use jsonrpsee::http_client::HttpClient;
use jsonrpsee::rpc_params;
use zns_resolver::net::Net;
use zns_verify::proof::{verify_chain, ProofLink};
use zns_verify::Action;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let [_, resolver_url, name, addr_reg] = &args[..4.min(args.len())] else {
        bail!("usage: verify_resolve <resolver-url> <name> <addr_reg-UA> [--regtest|--mainnet]");
    };
    let network = match args.get(4).map(String::as_str) {
        Some("--regtest") => Net::Regtest,
        Some("--mainnet") => Net::Main,
        _ => Net::Test,
    };

    // The registry's spec constants (PROOFS.md §4): the commitment keys come
    // from the *published* addr_reg, never from the resolver's response.
    let (g_d, pk_d) = registry_commitment_keys(&network, addr_reg)?;

    // 1. Ask the resolver — like any SDK client would.
    let client = HttpClient::builder().build(resolver_url)?;
    let answer: serde_json::Value =
        client.request("resolve", rpc_params![name, (), (), true]).await?;
    println!("resolver says: {} → {}", name, answer["address"].as_str().unwrap_or("?"));

    // 2. Parse the proof links.
    let links = answer["proof"]["links"]
        .as_array()
        .ok_or_else(|| anyhow!("resolver returned no proof (synced without --validator-rpc?)"))?
        .iter()
        .map(parse_link)
        .collect::<Result<Vec<_>>>()?;
    println!("proof: {} link(s)", links.len());

    // 3. Trust nothing: re-derive the whole chain from the proof material.
    let resolution = verify_chain(&network, name, &links, g_d, pk_d, 0)
        .map_err(|e| anyhow!("PROOF REJECTED: {e}"))?;

    match &resolution.ua {
        Some(ua) => println!("VERIFIED: {name} → {ua}"),
        None => println!("VERIFIED: {name} is released (tip is a RELEASE)"),
    }
    for a in &resolution.anchors {
        // A real wallet checks these against its own synced header chain;
        // print them so the operator can (block hashes display reversed).
        let mut display = a.block_hash;
        display.reverse();
        println!("  anchored @h{} under block {}", a.height, hex::encode(display));
    }
    Ok(())
}

/// `(g_d, pk_d)` of the registry's Orchard receiver, from the published UA.
fn registry_commitment_keys(network: &Net, ua: &str) -> Result<([u8; 32], [u8; 32])> {
    let addr = zcash_keys::address::Address::decode(network, ua)
        .ok_or_else(|| anyhow!("addr_reg does not decode for this network"))?;
    let zcash_keys::address::Address::Unified(ua) = addr else {
        bail!("addr_reg is not a Unified Address");
    };
    let orchard = ua.orchard().ok_or_else(|| anyhow!("addr_reg has no Orchard receiver"))?;
    Ok(orchard.zns_commitment_keys())
}

/// One wire link → the kernel's [`ProofLink`].
fn parse_link(v: &serde_json::Value) -> Result<ProofLink> {
    let s = |k: &str| v[k].as_str().ok_or_else(|| anyhow!("link field {k} missing"));
    let verb = s("action")?;
    let action =
        Action::from_bytes(verb.as_bytes()).ok_or_else(|| anyhow!("unknown action {verb:?}"))?;
    let branch = v["merkle_branch"]
        .as_array()
        .ok_or_else(|| anyhow!("merkle_branch missing"))?
        .iter()
        .map(|h| {
            let bytes = hex::decode(h.as_str().context("branch hex")?)?;
            bytes.try_into().map_err(|_| anyhow!("branch element length"))
        })
        .collect::<Result<Vec<[u8; 32]>>>()?;
    Ok(ProofLink {
        action,
        ua: s("ua")?.to_string(),
        height: v["height"].as_u64().context("height")? as u32,
        action_index: v["action_index"].as_u64().context("action_index")? as usize,
        tx: hex::decode(s("tx")?)?,
        header: hex::decode(s("header")?)?,
        merkle_branch: branch,
        merkle_index: v["merkle_index"].as_u64().context("merkle_index")? as u32,
    })
}
