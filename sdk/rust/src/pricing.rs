use crate::Error;

pub const ANNUAL_USD_TIERS: [u64; 5] = [10_000, 2_500, 800, 400, 100];
pub const ANNUAL_USD_MINIMUM: u64 = 20;
pub const FOREVER_MULTIPLE: u64 = 3;

/// Annual USD display price by name length. Empty input uses the first tier.
/// Valid protocol names are ASCII; UTF-16 length matches the Dart helper.
pub fn annual_usd(name: &str) -> u64 {
    let length = name.encode_utf16().count().max(1);
    ANNUAL_USD_TIERS
        .get(length - 1)
        .copied()
        .unwrap_or(ANNUAL_USD_MINIMUM)
}

/// Forever registration costs three annual prices.
pub fn forever_usd(name: &str) -> u64 {
    annual_usd(name) * FOREVER_MULTIPLE
}

/// Display-only conversion at an oracle's zats-per-USD rate, rounded up.
/// The mint computes the authoritative payment. Rejects negative, non-finite
/// or out-of-range values rather than silently saturating the integer cast.
pub fn quote_zats(usd: f64, zats_per_usd: f64) -> Result<u64, Error> {
    let quote = (usd * zats_per_usd).ceil();
    if !usd.is_finite()
        || !zats_per_usd.is_finite()
        || usd < 0.0
        || zats_per_usd < 0.0
        || !quote.is_finite()
        || quote >= u64::MAX as f64
    {
        return Err(Error::InvalidInput(
            "price and rate must yield a finite nonnegative u64 quote",
        ));
    }
    Ok(quote as u64)
}
