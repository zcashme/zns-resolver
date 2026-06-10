//! Proof-material acquisition — the resolver-side half of `PROOFS.md`.
//!
//! The scan loop runs on lightwalletd compact blocks, which cannot yield
//! Merkle branches (they omit transparent-only txids). So proof context comes
//! from a validator JSON-RPC endpoint (zebrad/zcashd): per Name Note block,
//! `getblock(height, 1)` for the txid list and `getblock(height, 0)` for the
//! raw header. The branch is computed here and stored alongside the raw
//! transaction (already in hand from note recovery) in `proof_material`.
//!
//! The serving side joins this material onto a name's chain rows; the
//! *verifying* fold lives in `zns_verify::proof` — the resolver builds
//! branches, the kernel checks them, and the two are cross-tested.

use anyhow::{anyhow, Context, Result};
use jsonrpsee::core::client::ClientT;
use jsonrpsee::http_client::HttpClient;
use jsonrpsee::rpc_params;
use sha2::{Digest, Sha256};

/// A minimal validator JSON-RPC client (zebrad or zcashd).
pub struct ValidatorClient {
    client: HttpClient,
}

/// One block's proof context: its raw header and every txid in Merkle-tree
/// (internal byte) order.
pub struct BlockContext {
    /// The raw serialized block header.
    pub header: Vec<u8>,
    /// All txids in the block, internal byte order, tree order.
    pub txids: Vec<[u8; 32]>,
}

impl ValidatorClient {
    /// Connect to a validator RPC endpoint, e.g. `http://127.0.0.1:8232`.
    pub fn new(url: &str) -> Result<Self> {
        let client = HttpClient::builder().build(url).context("validator RPC url")?;
        Ok(Self { client })
    }

    /// Fetch the proof context for the block at `height`.
    pub async fn block_context(&self, height: u32) -> Result<BlockContext> {
        let arg = height.to_string();

        // Verbosity 1: the txid list (display-order hex; reverse to internal).
        let info: serde_json::Value = self
            .client
            .request("getblock", rpc_params![&arg, 1])
            .await
            .with_context(|| format!("getblock {height} (verbose)"))?;
        let txids = info
            .get("tx")
            .and_then(|t| t.as_array())
            .ok_or_else(|| anyhow!("getblock {height}: no tx list"))?
            .iter()
            .map(|v| {
                let hex_str = v.as_str().ok_or_else(|| anyhow!("non-string txid"))?;
                let mut bytes: [u8; 32] =
                    hex::decode(hex_str)?.try_into().map_err(|_| anyhow!("txid length"))?;
                bytes.reverse(); // display order → internal order
                Ok(bytes)
            })
            .collect::<Result<Vec<_>>>()?;

        // Verbosity 0: the raw block; the header is its prefix. Parsing via
        // BlockHeader bounds the read (the equihash solution is length-
        // prefixed), then re-serializing yields exactly the header bytes.
        let raw_hex: String = self
            .client
            .request("getblock", rpc_params![&arg, 0])
            .await
            .with_context(|| format!("getblock {height} (raw)"))?;
        let raw = hex::decode(raw_hex.trim()).context("raw block hex")?;
        let parsed = zcash_primitives::block::BlockHeader::read(&raw[..])
            .with_context(|| format!("block {height} header parse"))?;
        let mut header = Vec::new();
        parsed.write(&mut header)?;

        Ok(BlockContext { header, txids })
    }
}

/// Build the Merkle branch for the leaf at `index` (Bitcoin-style tree:
/// double-SHA256 pairs, odd level-ends duplicated). Returns the siblings
/// leaf → root; an empty branch means a single-tx block.
///
/// The verifying fold is `zns_verify::proof::merkle_fold`; the two are
/// cross-tested in this module.
pub fn merkle_branch(txids: &[[u8; 32]], index: usize) -> Vec<[u8; 32]> {
    assert!(index < txids.len(), "leaf index in range");
    let mut level: Vec<[u8; 32]> = txids.to_vec();
    let mut idx = index;
    let mut branch = Vec::new();
    while level.len() > 1 {
        if level.len() % 2 == 1 {
            level.push(*level.last().expect("non-empty"));
        }
        let sibling = if idx % 2 == 1 { level[idx - 1] } else { level[idx + 1] };
        branch.push(sibling);
        level = level
            .chunks_exact(2)
            .map(|pair| sha256d(&pair[0], &pair[1]))
            .collect();
        idx /= 2;
    }
    branch
}

fn sha256d(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let first = Sha256::new().chain_update(left).chain_update(right).finalize();
    Sha256::digest(first).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use zns_verify::proof::merkle_fold;

    fn txids(n: usize) -> Vec<[u8; 32]> {
        (0..n).map(|i| [i as u8; 32]).collect()
    }

    /// The builder and the kernel's verifying fold must agree for every leaf
    /// of every tree shape, including odd levels (duplicated last element).
    #[test]
    fn branch_round_trips_through_kernel_fold() {
        for n in 1..=8 {
            let ids = txids(n);
            let root = merkle_root(&ids);
            for (i, id) in ids.iter().enumerate() {
                let branch = merkle_branch(&ids, i);
                assert_eq!(
                    merkle_fold(*id, &branch, i as u32),
                    root,
                    "n={n} leaf={i}"
                );
            }
        }
    }

    /// Reference root: fold the whole level structure.
    fn merkle_root(txids: &[[u8; 32]]) -> [u8; 32] {
        let mut level = txids.to_vec();
        while level.len() > 1 {
            if level.len() % 2 == 1 {
                level.push(*level.last().unwrap());
            }
            level = level.chunks_exact(2).map(|p| sha256d(&p[0], &p[1])).collect();
        }
        level[0]
    }

    #[test]
    fn wrong_leaf_does_not_fold_to_root() {
        let ids = txids(4);
        let root = merkle_root(&ids);
        let branch = merkle_branch(&ids, 2);
        assert_ne!(merkle_fold([0xff; 32], &branch, 2), root);
        // Right leaf, wrong position.
        assert_ne!(merkle_fold(ids[2], &branch, 3), root);
    }
}
