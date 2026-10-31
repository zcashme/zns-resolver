/// Validate a lowercase unified address by its allowed length and Bech32m
/// checksum. This does not decode receivers or establish Orchard support.
pub fn is_valid_unified_address(address: &str) -> bool {
    let lengths = if address.starts_with("utest1") {
        [110, 145, 182, 217]
    } else {
        [106, 141, 178, 213]
    };
    if !lengths.contains(&address.len()) || !address.is_ascii() {
        return false;
    }
    let Some((hrp, data)) = address.rsplit_once('1') else {
        return false;
    };
    if hrp != "u" && hrp != "utest" {
        return false;
    }
    const CHARSET: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
    const GENERATORS: [u32; 5] = [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3];
    let mut values: Vec<u32> = hrp.bytes().map(|b| u32::from(b >> 5)).collect();
    values.push(0);
    values.extend(hrp.bytes().map(|b| u32::from(b & 31)));
    for byte in data.bytes() {
        let Some(value) = CHARSET.iter().position(|&b| b == byte) else {
            return false;
        };
        values.push(value as u32);
    }
    let checksum = values.into_iter().fold(1u32, |checksum, value| {
        let top = checksum >> 25;
        let mut next = ((checksum & 0x1ffffff) << 5) ^ value;
        for (i, generator) in GENERATORS.iter().enumerate() {
            if (top >> i) & 1 == 1 {
                next ^= generator;
            }
        }
        next
    });
    checksum == 0x2bc830a3
}
