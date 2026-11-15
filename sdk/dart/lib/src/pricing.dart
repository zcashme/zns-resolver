/// The ZNS pricing law, mirroring zns-mint/mint/src/mint/pricing.rs.
///
/// These constants are the display-side source of truth for what a name
/// costs. They are baked in on purpose: pricing is protocol identity, not
/// deployment config. The mint remains the only charging authority: it
/// computes the authoritative ZEC amount at payment time.
///
/// Prices are USD. Converting to zats for display needs the oracle's daily
/// zats-per-USD rate (see [quoteZats]); no rate is baked in here.
library;

/// Annual USD price by name length, for 1-9 character names.
const Map<int, int> znsAnnualUsdTiers = {
  1: 12480,
  2: 2720,
  3: 920,
  4: 472,
  5: 188,
  6: 112,
  7: 48,
  8: 32,
  9: 24,
};

/// Annual USD price for a name of 10 or more characters.
const int znsAnnualUsdMinimum = 18;

/// A forever registration costs this multiple of the annual price.
const int znsForeverMultiple = 4;

/// Annual USD price for [name]. An empty name is priced at the 1-character
/// tier.
int annualUsd(String name) {
  final len = name.isEmpty ? 1 : name.length;
  return znsAnnualUsdTiers[len] ?? znsAnnualUsdMinimum;
}

/// Forever (one-time) USD price for [name].
int foreverUsd(String name) => annualUsd(name) * znsForeverMultiple;

/// Convert a USD price to zats at the given oracle rate (zats per USD).
/// Display only: the mint computes the authoritative charge. Rounds up so a
/// quote never undercuts the price.
int quoteZats(num usd, num zatsPerUsd) => (usd * zatsPerUsd).ceil();
