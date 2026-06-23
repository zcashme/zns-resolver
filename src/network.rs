//! Compile-time network selection.
//!

#[cfg(all(feature = "mainnet", feature = "testnet"))]
compile_error!("mainnet and testnet are mutually exclusive");

#[cfg(not(any(feature = "mainnet", feature = "testnet")))]
compile_error!("enable either mainnet (default) or testnet feature");

#[cfg(any(feature = "mainnet", feature = "testnet"))]
use zcash_protocol::consensus::Network;

/// Registry unified full viewing key for the active network.
#[cfg(feature = "mainnet")]
pub const UFVK: &str = "ufvk1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq"; // TODO: replace with real mainnet registry UFVK before building for mainnet

#[cfg(feature = "testnet")]
pub const UFVK: &str = "ufvktest18a7ht78cymvm3sxdw9myrr04nrnj8nvrqdjhadj8dp3cv8pm2dqszuxnjrjyp6xyf0svtzjxnq3976l5sxzd09mmx9g6sj9xpp67ympwsrv6wen5ye25jhvq0l8zz937hcgtp90rwhjq0m02rf7qk6wmvrny26r2vt0laztqx4kgx0jqtdwu38ld0hx53m0u20rjny20gpxneavfze7aqqft5vs0jraaqed4974avkx4c3qass3prsqq2fdx08jllet4uuxzz8zmrem8xcwaya9v50l046lp2c9uuyrkp0r8zz937hcgtp90rwhjq0m02rf7qk6wmvrny26r2vt0laztqx4kgx0jqtdwu38ld0hx53m0u20rjny20gpxneavfze7aqqft5vs0jraaqed4974avkx4c3qass3prsqq2fdx08jllet4uuxzz8zmrem8xcwaya9v50l046lp2c9uuyrkp0r8jja5vlzday32pgq4cccqd2rjvtlsfnn9lne9cchrcfgn87jlx9";

/// Zcash consensus parameters for the active network.
#[cfg(feature = "mainnet")]
pub const NETWORK: Network = Network::MainNetwork;

#[cfg(feature = "testnet")]
pub const NETWORK: Network = Network::TestNetwork;

/// Persisted name index filename. Distinct per network so mainnet and testnet
/// builds do not share on-disk state.
#[cfg(feature = "mainnet")]
pub const DB_PATH: &str = "zns.sqlite";

#[cfg(feature = "testnet")]
pub const DB_PATH: &str = "zns-testnet.sqlite";

/// Skip all blocks before this height on first sync (performance).
#[cfg(feature = "mainnet")]
pub const SCAN_BIRTHDAY: u32 = 3000000

#[cfg(feature = "testnet")]
pub const SCAN_BIRTHDAY: u32 = 4_000_000;
