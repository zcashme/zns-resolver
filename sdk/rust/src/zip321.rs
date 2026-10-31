use base64::{
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD},
    Engine,
};

use crate::Error;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Zip321Parts {
    pub address: String,
    /// Decimal ZEC, not zatoshis. Empty when absent.
    pub amount: String,
    pub memo_raw: String,
    pub memo_decoded: String,
}

/// Base64url UTF-8 text without padding.
pub fn to_base64_url(text: &str) -> String {
    URL_SAFE_NO_PAD.encode(text)
}

/// Decode padded or unpadded base64url. Invalid base64 or UTF-8 yields empty text.
pub fn decode_base64_url(value: &str) -> String {
    [URL_SAFE_NO_PAD, URL_SAFE, STANDARD_NO_PAD, STANDARD]
        .iter()
        .find_map(|engine| engine.decode(value).ok())
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_default()
}

fn valid_amount(amount: &str) -> bool {
    let (whole, fraction) = match amount.split_once('.') {
        Some((whole, fraction)) => {
            if fraction.is_empty()
                || fraction.len() > 8
                || !fraction.bytes().all(|b| b.is_ascii_digit())
            {
                return false;
            }
            (whole, fraction)
        }
        None => (amount, ""),
    };
    if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let whole = whole.trim_start_matches('0');
    // Compare decimal strings to avoid overflow and floating-point rounding.
    whole.len() < 8
        || (whole.len() == 8
            && (whole < "21000000" || (whole == "21000000" && fraction.bytes().all(|b| b == b'0'))))
}

/// Single-recipient ZIP-321 URI. Amounts have at most eight decimal places
/// and cannot exceed 21,000,000 ZEC. Memos are at most 512 UTF-8 bytes.
pub fn build_zip321_uri(
    address: &str,
    amount: Option<&str>,
    memo: Option<&str>,
) -> Result<String, Error> {
    // Zcash addresses (Bech32, Bech32m, Base58Check) are ASCII letters and
    // digits only, so no URI delimiter (`?`, `&`, `#`, ...) can slip through.
    if address.is_empty() || !address.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(Error::InvalidInput(
            "a payment URI needs an address of ASCII letters and digits",
        ));
    }
    let mut params = Vec::new();
    if let Some(amount) = amount {
        if !valid_amount(amount) {
            return Err(Error::InvalidInput(
                "amount must be decimal ZEC with at most 8 decimals, at most 21000000 ZEC",
            ));
        }
        params.push(format!("amount={amount}"));
    }
    if let Some(memo) = memo.filter(|memo| !memo.is_empty()) {
        if address.starts_with('t') {
            return Err(Error::InvalidInput(
                "a transparent address cannot receive a memo",
            ));
        }
        if memo.len() > 512 {
            return Err(Error::InvalidInput("the memo is more than 512 UTF-8 bytes"));
        }
        params.push(format!("memo={}", to_base64_url(memo)));
    }
    Ok(if params.is_empty() {
        format!("zcash:{address}")
    } else {
        format!("zcash:{address}?{}", params.join("&"))
    })
}

// Match URLSearchParams: leave malformed percent escapes literal and replace
// invalid UTF-8, without discarding other query fields.
fn decode_component(component: &str) -> String {
    let mut bytes = Vec::new();
    let input = component.as_bytes();
    let mut index = 0;
    while index < input.len() {
        if input[index] == b'%' && index + 2 < input.len() {
            if let (Some(high), Some(low)) = (
                (input[index + 1] as char).to_digit(16),
                (input[index + 2] as char).to_digit(16),
            ) {
                bytes.push((high * 16 + low) as u8);
                index += 3;
                continue;
            }
        }
        bytes.push(match input[index] {
            b'+' => b' ',
            other => other,
        });
        index += 1;
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Parse one payment URI, tolerating a missing or case-insensitive scheme.
/// Missing fields and undecodable memos are empty. Like TypeScript's
/// URLSearchParams, keeps the first duplicate field and tolerates malformed
/// query encoding without discarding other payment fields. When the URI has no
/// address before `?`, the `address` query field is used, as ZIP 321 allows.
pub fn parse_zip321_uri(uri: &str) -> Zip321Parts {
    let uri = if uri
        .get(..6)
        .is_some_and(|s| s.eq_ignore_ascii_case("zcash:"))
    {
        &uri[6..]
    } else {
        uri
    };
    let (address, query) = uri.split_once('?').unwrap_or((uri, ""));
    let mut address_field = None;
    let mut amount = None;
    let mut memo_raw = None;
    for param in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = param.split_once('=').unwrap_or((param, ""));
        let key = decode_component(key);
        let value = decode_component(value);
        match key.as_str() {
            "address" if address_field.is_none() => address_field = Some(value.trim().to_owned()),
            "amount" if amount.is_none() => amount = Some(value.trim().to_owned()),
            "memo" if memo_raw.is_none() => memo_raw = Some(value.trim().to_owned()),
            _ => {}
        }
    }
    let memo_raw = memo_raw.unwrap_or_default();
    let address = match address.trim() {
        "" => address_field.unwrap_or_default(),
        address => address.into(),
    };
    Zip321Parts {
        address,
        amount: amount.unwrap_or_default(),
        memo_decoded: decode_base64_url(&memo_raw),
        memo_raw,
    }
}
