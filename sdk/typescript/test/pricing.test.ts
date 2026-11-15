import { describe, it, expect } from "vitest";
import { ZNS_PRICING, annualUsd, foreverUsd, quoteZats } from "../src/pricing.js";

describe("ZNS_PRICING", () => {
  it("matches the mint's pricing law", () => {
    expect(ZNS_PRICING.annualUsd).toEqual({
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
    });
    expect(ZNS_PRICING.foreverMultiple).toBe(4);
  });
});

describe("annualUsd", () => {
  it("prices every tier", () => {
    const tiers = [12_480, 2_720, 920, 472, 188, 112, 48, 32, 24];
    tiers.forEach((usd, i) => expect(annualUsd("a".repeat(i + 1))).toBe(usd));
    expect(annualUsd("a".repeat(10))).toBe(18); // 10 chars — first minimum tier
    expect(annualUsd("a".repeat(63))).toBe(18); // 63 chars — max length
  });

  it("clamps empty input to the 1-char tier", () => {
    expect(annualUsd("")).toBe(12_480);
  });
});

describe("foreverUsd", () => {
  it("charges four annual prices, one time", () => {
    expect(foreverUsd("bob")).toBe(3_680); // 3-char: 920 × 4
    expect(foreverUsd("alice")).toBe(752); // 5-char: 188 × 4
    expect(foreverUsd("a".repeat(10))).toBe(72); // minimum: 18 × 4
  });
});

describe("quoteZats", () => {
  it("converts USD at the given rate", () => {
    expect(quoteZats(920, 1_000)).toBe(920_000); // 920 USD at 1000 zats/USD
  });

  it("rounds up so quotes never undercut the price", () => {
    expect(quoteZats(18, 333)).toBe(5_994);
    expect(quoteZats(1, 0.5)).toBe(1);
  });
});
