/**
 * The ZNS pricing law — mirrors zns-mint/src/mint/pricing.rs.
 *
 * These constants are the display-side source of truth for what a name
 * costs. They are baked in on purpose: pricing is protocol identity, not
 * deployment config. The mint remains the only charging authority — it
 * computes the authoritative ZEC amount on-chain at payment time.
 *
 * Prices are USD. Converting to zats for display requires the oracle's
 * daily zats-per-USD rate (see {@link quoteZats}); no rate is baked here.
 */

export const ZNS_PRICING = {
  /** Annual USD price by name length: tiers cover 1–5 character names and
   *  6+ character names pay the minimum. */
  annualUsd: { 1: 10_000, 2: 2_500, 3: 800, 4: 400, 5: 100, minimum: 20 },
  /** Forever registration costs this multiple of the annual price. */
  foreverMultiple: 3,
} as const;

/** Annual USD price for a name. The lookup clamps to the 1-char tier. */
export function annualUsd(name: string): number {
  const len = Math.max(1, name.length);
  return len <= 5
    ? ZNS_PRICING.annualUsd[len as 1 | 2 | 3 | 4 | 5]
    : ZNS_PRICING.annualUsd.minimum;
}

/** Forever (one-time) USD price for a name: three annual prices. */
export function foreverUsd(name: string): number {
  return annualUsd(name) * ZNS_PRICING.foreverMultiple;
}

/** Convert a USD price to zats at the given oracle rate (zats per USD).
 *  Display-only — the mint computes the authoritative charge on-chain.
 *  Rounds up so quoted amounts never undercut the price. */
export function quoteZats(usd: number, zatsPerUsd: number): number {
  return Math.ceil(usd * zatsPerUsd);
}
