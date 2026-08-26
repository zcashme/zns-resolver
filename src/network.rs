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
pub const UFVK: &str = "uviewtest1m6ttk6khq8gy0s5v5e5c9snavnwzyv9hl9d5g7kc9lczlv36mjj4tpkmqqd5jep4cg0ea79ahqjpz3huv28kp2frtr3vc9wgerseynuntyu92ky6nwd746w8waz7jv34ax32h4uffcj7ky8qphxesmqqzvt7ykdle5lg2vv69we9nz2q89m8pudjzngxk82mh2s3p3uqedjucnca95tzdqqsg7pn5htvulp8hcyhqa8t4qhlxnpqw7elupkeyvzwky4lta26yy4tvgqz5pjx6ew9e3hm4wmu5t4jt7ku450atn83fezs6r5mc6jkxjc4xcptzss3c3e8ldrnj0uru9tnjteelxzzx7mzrwetu965t2z8luz24h9cj37g9q5nclyczp4gnx2g5z4twlkl9mtvdxwdwxza7chztzcgw6e4eye36auh6p5ltzclppxykhmalghf0fk8087jhknjyzxfzkukj4fmt3umm0k27mh44lfxmc8m0kvh";

/// Zcash consensus parameters for the active network.
#[cfg(feature = "mainnet")]
pub const NETWORK: Network = Network::MainNetwork;

#[cfg(feature = "testnet")]
pub const NETWORK: Network = Network::TestNetwork;

/// Short name of the active network for logging.
#[cfg(feature = "mainnet")]
pub const NETWORK_NAME: &str = "main";

#[cfg(feature = "testnet")]
pub const NETWORK_NAME: &str = "test";

/// Persisted name index filename. Distinct per network so mainnet and testnet
/// builds do not share on-disk state.
#[cfg(feature = "mainnet")]
pub const DB_PATH: &str = "zns.sqlite";

#[cfg(feature = "testnet")]
pub const DB_PATH: &str = "zns-testnet.sqlite";

/// Skip all blocks before this height on first sync (performance).
#[cfg(feature = "mainnet")]
pub const SCAN_BIRTHDAY: u32 = 3000000;

#[cfg(feature = "testnet")]
pub const SCAN_BIRTHDAY: u32 = 4_000_000;
