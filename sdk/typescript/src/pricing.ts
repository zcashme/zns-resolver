/**
 * The ZNS pricing law — mirrors zns-mint/mint/src/mint/pricing.rs.
 *
 * These constants are the display-side source of truth for what a name
 * costs. They are baked in on purpose: pricing is protocol identity, not
 * deployment config. The mint remains the only charging authority — it
 * computes the authoritative ZEC amount on-chain at payment time.
 *
 * Prices are USD. Converting to zats for display requires the oracle's
 * zats-per-USD rate (see {@link quoteZats}); no rate is baked here.
 */

export const ZNS_PRICING = {
  /** Annual USD price by name length: tiers cover 1–9 character names and
   *  10+ character names pay the minimum. */
  annualUsd: {
    1: 12_480,
    2: 2_720,
    3: 920,
    4: 472,
    5: 188,
    6: 112,
    7: 48,
    8: 32,
    9: 24,
    minimum: 18,
  },
  /** Forever registration costs this multiple of the annual price. */
  foreverMultiple: 4,
} as const;

type Tier = 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9;

/** Annual USD price for a name. The lookup clamps to the 1-char tier. */
export function annualUsd(name: string): number {
  const len = Math.max(1, name.length);
  return len <= 9 ? ZNS_PRICING.annualUsd[len as Tier] : ZNS_PRICING.annualUsd.minimum;
}

/** Forever (one-time) USD price for a name. */
export function foreverUsd(name: string): number {
  return annualUsd(name) * ZNS_PRICING.foreverMultiple;
}

/** Convert a USD price to zats at the given oracle rate (zats per USD).
 *  Display-only — the mint computes the authoritative charge on-chain.
 *  Rounds up so quoted amounts never undercut the price. */
export function quoteZats(usd: number, zatsPerUsd: number): number {
  return Math.ceil(usd * zatsPerUsd);
}
