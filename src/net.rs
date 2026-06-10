//! Network parameterization: mainnet, testnet, or the local NU6.2 regtest.
//!
//! `zcash_protocol::consensus::Network` only models the two public chains;
//! the regtest variant carries the activation heights our
//! `zebra-regtest/zebrad.toml` configures, so `BranchId::for_height` resolves
//! correctly on the local chain and `uivkregtest1…` keys decode.

use zcash_protocol::consensus::{
    BlockHeight, Network, NetworkType, NetworkUpgrade, Parameters,
};
use zcash_protocol::local_consensus::LocalNetwork;

/// The chain this resolver follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Net {
    /// Zcash mainnet.
    Main,
    /// Public testnet.
    Test,
    /// The local NU6.2 regtest chain (`zebra-regtest/zebrad.toml`):
    /// Overwinter…NU6 at height 1, NU6.1 at 20, NU6.2 at 22.
    Regtest,
}

/// The regtest activation heights — must agree with `zebrad.toml` (and with
/// `zns-core`'s `ZcashNetwork::Regtest` on the mint side).
const REGTEST: LocalNetwork = LocalNetwork {
    overwinter: Some(BlockHeight::from_u32(1)),
    sapling: Some(BlockHeight::from_u32(1)),
    blossom: Some(BlockHeight::from_u32(1)),
    heartwood: Some(BlockHeight::from_u32(1)),
    canopy: Some(BlockHeight::from_u32(1)),
    nu5: Some(BlockHeight::from_u32(1)),
    nu6: Some(BlockHeight::from_u32(1)),
    nu6_1: Some(BlockHeight::from_u32(20)),
    nu6_2: Some(BlockHeight::from_u32(22)),
};

impl Net {
    /// The default scan birthday: NU5 activation (earliest possible Orchard
    /// note) on the public chains, genesis on regtest.
    pub fn default_birthday(self) -> u32 {
        match self {
            Net::Main => crate::MAINNET_NU5_HEIGHT,
            Net::Test => crate::TESTNET_NU5_HEIGHT,
            Net::Regtest => 1,
        }
    }
}

impl Parameters for Net {
    fn network_type(&self) -> NetworkType {
        match self {
            Net::Main => NetworkType::Main,
            Net::Test => NetworkType::Test,
            Net::Regtest => NetworkType::Regtest,
        }
    }

    fn activation_height(&self, nu: NetworkUpgrade) -> Option<BlockHeight> {
        match self {
            Net::Main => Network::MainNetwork.activation_height(nu),
            Net::Test => Network::TestNetwork.activation_height(nu),
            Net::Regtest => REGTEST.activation_height(nu),
        }
    }
}
