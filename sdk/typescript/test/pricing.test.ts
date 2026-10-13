import { describe, it, expect } from "vitest";
import { ZNS_PRICING, annualUsd, foreverUsd, quoteZats } from "../src/pricing.js";

describe("ZNS_PRICING", () => {
  it("matches the mint's pricing law", () => {
    expect(ZNS_PRICING.annualUsd).toEqual({
      1: 10_000,
      2: 2_500,
      3: 800,
      4: 400,
      5: 100,
      minimum: 20,
    });
    expect(ZNS_PRICING.foreverMultiple).toBe(3);
  });
});

describe("annualUsd", () => {
  it("prices every tier", () => {
    expect(annualUsd("a")).toBe(10_000); // 1 char
    expect(annualUsd("ab")).toBe(2_500); // 2 chars
    expect(annualUsd("abc")).toBe(800); // 3 chars
    expect(annualUsd("abcd")).toBe(400); // 4 chars
    expect(annualUsd("abcde")).toBe(100); // 5 chars
    expect(annualUsd("abcdef")).toBe(20); // 6 chars — first minimum tier
    expect(annualUsd("a".repeat(63))).toBe(20); // 63 chars — max length
  });

  it("clamps empty input to the 1-char tier", () => {
    expect(annualUsd("")).toBe(10_000);
  });
});

describe("foreverUsd", () => {
  it("charges three annual prices, one time", () => {
    expect(foreverUsd("bob")).toBe(2_400); // 3-char: 800 × 3
    expect(foreverUsd("alice")).toBe(300); // 5-char: 100 × 3
    expect(foreverUsd("a".repeat(6))).toBe(60); // minimum: 20 × 3
  });
});

describe("quoteZats", () => {
  it("converts USD at the given rate", () => {
    expect(quoteZats(800, 1_000)).toBe(800_000); // 800 USD at 1000 zats/USD
  });

  it("rounds up so quotes never undercut the price", () => {
    expect(quoteZats(20, 333)).toBe(6_660);
    expect(quoteZats(1, 0.5)).toBe(1);
  });
});
