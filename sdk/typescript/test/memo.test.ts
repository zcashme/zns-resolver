import { describe, it, expect } from "vitest";
import {
  buildClaimMemo,
  buildUpdateMemo,
  buildReleaseMemo,
  buildOtpMemo,
  parseOtpMemo,
  buildListMemo,
  buildAuctionMemo,
  buildDelistMemo,
  buildBuyMemo,
  parseBuyOtpMemo,
  buyPaymentZats,
  buildZip321Uri,
  parseZip321Uri,
} from "../src/zns.js";

// zns-mint/src/mint.rs test vector: a mainnet UA with Orchard, Sapling and
// transparent receivers.
const UA = "u1d398kq0gfmegkvn0c57zmvq7gcnhxs6g3chfewlxq2yzhdjpx7uk3h80qgku5ygtyr9m7y6swgqe3pqdleu5uvwmangjj8yk7s5j0u78frtw9y9y5lx4c0x3cp054m9nl274xynwf5ad2uah7afyu4wgu3mwg5xvq4zmrdcplt8uqeqqw4vu4kdwngzvsn7gtdwtx3whkwt4z20pr0k";
// Live testnet treasury: Orchard and Sapling receivers.
const TESTNET_UA = "utest1tquw9k9qlsphhf30tau2qh758vfvf7xkvjea3rlc5mj4cjscgzfzgjeafjnl7r8tvv0akq3v2ze5q0azf4lnd4vf7kusrfensdsrr7tsgruvuqk0dv0hepnrk0pzgz3fj0mxc0lr29yf65nkwvw74gzep4792rkqa769lxl0sua09s0r";

describe("mint request memos", () => {
  it("builds the formats the mint parses", () => {
    expect(buildClaimMemo({ name: "alice", ua: UA, term: "forever" }))
      .toBe(`ZNS:claim:forever:alice:${UA}`);
    expect(buildClaimMemo({ name: "alice", ua: UA, term: { years: 12 } }))
      .toBe(`ZNS:claim:12y:alice:${UA}`);
    expect(buildClaimMemo({ name: "alice", ua: UA, term: "forever", accessCode: "004206" }))
      .toBe(`ZNS:claim:004206:forever:alice:${UA}`);
    expect(buildUpdateMemo({ name: "alice", ua: UA }))
      .toBe(`ZNS:update:none:alice:${UA}`);
    expect(buildUpdateMemo({ name: "alice", ua: UA, term: "forever" }))
      .toBe(`ZNS:update:forever:alice:${UA}`);
    expect(buildReleaseMemo({ name: "alice", ua: UA }))
      .toBe(`ZNS:release:alice:${UA}`);
    expect(buildOtpMemo({ code: "417293", name: "alice", action: "update", ua: UA }))
      .toBe(`ZNS:otp:417293:alice:update:${UA}`);
  });

  it("accepts a testnet UA", () => {
    expect(buildReleaseMemo({ name: "alice", ua: TESTNET_UA }))
      .toBe(`ZNS:release:alice:${TESTNET_UA}`);
  });

  it("throws on input the mint refuses", () => {
    const ok = { name: "alice", ua: UA, term: "forever" as const };
    expect(() => buildClaimMemo({ ...ok, accessCode: "4206" })).toThrow(/six digits/);
    expect(() => buildClaimMemo({ ...ok, accessCode: "a1b2c3" })).toThrow(/six digits/);
    expect(() => buildClaimMemo({ ...ok, name: "INVALID" })).toThrow(/name/);
    for (const years of [0, 100, 1.5]) {
      expect(() => buildClaimMemo({ ...ok, term: { years } })).toThrow(/term/);
    }
    for (const ua of [UA.slice(0, -1), `${UA}q`, UA.toUpperCase(), `${UA.slice(0, -1)}:`, "t1abc", "zs1abc", ""]) {
      expect(() => buildClaimMemo({ ...ok, ua })).toThrow(/unified address/);
    }
    // One mistyped character, still in the bech32 alphabet: the checksum fails.
    const typo = UA.slice(0, 50) + (UA[50] === "q" ? "p" : "q") + UA.slice(51);
    expect(() => buildClaimMemo({ ...ok, ua: typo })).toThrow(/unified address/);
    expect(() =>
      buildOtpMemo({ code: "417293", name: "alice", action: "claim" as never, ua: UA }),
    ).toThrow(/only updates, releases, listings and delistings/);
  });
});

// A mainnet public-key-hash address: where a buyer pays.
const T1 = "t1Hsc1LR8yKnbbe3twRp88p6vFfC5t7DLbs";

describe("marketplace memos", () => {
  const auction = { name: "alice", ua: UA, payTo: T1, startZats: 500_000_000, minZats: 150_000_000, startBlock: 3_600_000, endBlock: 3_610_000 };

  it("builds the formats the mint parses", () => {
    expect(buildListMemo({ name: "alice", ua: UA, payTo: T1, priceZats: 250_000_000 }))
      .toBe(`ZNS:list:alice:${UA}:${T1}:2.5`);
    expect(buildListMemo({ name: "alice", ua: UA, payTo: T1, priceZats: 1 }))
      .toBe(`ZNS:list:alice:${UA}:${T1}:0.00000001`);
    expect(buildAuctionMemo(auction))
      .toBe(`ZNS:auction:alice:${UA}:${T1}:5:1.5:3600000:3610000`);
    expect(buildDelistMemo({ name: "alice", ua: UA })).toBe(`ZNS:delist:alice:${UA}`);
    expect(buildBuyMemo({ name: "alice", ua: UA })).toBe(`ZNS:buy:alice:${UA}`);
  });

  it("throws on terms the mint refuses", () => {
    const list = { name: "alice", ua: UA, payTo: T1, priceZats: 1 };
    expect(() => buildListMemo({ ...list, payTo: UA })).toThrow(/t1/);
    // A testnet address for a mainnet name.
    expect(() => buildListMemo({ ...list, payTo: "tmEZhbWHTpdKMw5it8YDspUXSMGQyFwovpU" })).toThrow(/t1/);
    for (const priceZats of [-1, 1.5, 21_000_001 * 100_000_000]) {
      expect(() => buildListMemo({ ...list, priceZats })).toThrow(/zatoshis/);
    }
    expect(() => buildAuctionMemo({ ...auction, endBlock: auction.startBlock })).toThrow(/ends after/);
    expect(() => buildAuctionMemo({ ...auction, minZats: auction.startZats })).toThrow(/falls/);
  });

  it("reads back list and auction challenges with their terms", () => {
    const list = { code: "417293", name: "alice", action: "list" as const, ua: UA, terms: { payTo: T1, ask: { kind: "flat" as const, priceZats: 250_000_000 } } };
    expect(buildOtpMemo(list)).toBe(`ZNS:otp:417293:alice:list:${UA}:${T1}:2.5`);
    expect(parseOtpMemo(buildOtpMemo(list))).toEqual(list);
    const { name, ua, payTo, ...ask } = auction;
    const dutch = { code: "417293", name, action: "auction" as const, ua, terms: { payTo, ask: { kind: "dutch" as const, ...ask } } };
    expect(parseOtpMemo(buildOtpMemo(dutch))).toEqual(dutch);
    expect(parseOtpMemo(`ZNS:otp:417293:alice:delist:${UA}`))
      .toEqual({ code: "417293", name: "alice", action: "delist", ua: UA });
  });

  it("refuses a challenge whose terms do not fit its verb", () => {
    expect(parseOtpMemo(`ZNS:otp:417293:alice:list:${UA}`)).toBeNull();
    expect(parseOtpMemo(`ZNS:otp:417293:alice:list:${UA}:${T1}:5:1.5:3600000:3610000`)).toBeNull();
    expect(parseOtpMemo(`ZNS:otp:417293:alice:auction:${UA}:${T1}:1.5:5:3600000:3610000`)).toBeNull();
    expect(parseOtpMemo(`ZNS:otp:417293:alice:delist:${UA}:${T1}:2.5`)).toBeNull();
    expect(parseOtpMemo(`ZNS:otp:417293:alice:list:${UA}:${T1}:02.5`)).toBeNull();
    expect(() => buildOtpMemo({ code: "417293", name: "alice", action: "update", ua: UA, terms: { payTo: T1, ask: { kind: "flat", priceZats: 1 } } }))
      .toThrow(/terms/);
  });

  it("reads the mint's answer to a buy", () => {
    expect(parseBuyOtpMemo(`ZNS:buyotp:004206:alice:${UA}`))
      .toEqual({ code: "004206", name: "alice", ua: UA });
    expect(parseBuyOtpMemo(`ZNS:dutch:004206:alice:${UA}:3.25:1763215200` + "\0".repeat(8)))
      .toEqual({ code: "004206", name: "alice", ua: UA, dutch: { priceZats: 325_000_000, until: 1_763_215_200 } });
    expect(parseBuyOtpMemo(`ZNS:otp:004206:alice:update:${UA}`)).toBeNull();
    expect(parseBuyOtpMemo(`ZNS:buyotp:004206:alice:${UA}:3.25`)).toBeNull();
    expect(parseBuyOtpMemo(`ZNS:dutch:004206:alice:${UA}:3.25`)).toBeNull();
    // An expiry past 2^53 - 1 cannot be held exactly, so it is refused.
    expect(parseBuyOtpMemo(`ZNS:dutch:004206:alice:${UA}:3.25:9007199254740992`)).toBeNull();
    expect(parseBuyOtpMemo(`ZNS:dutch:004206:alice:${UA}:3.25:${"9".repeat(40)}`)).toBeNull();
  });

  it("pays the price plus the buy code in zatoshis", () => {
    expect(buyPaymentZats(250_000_000, "004206")).toBe(250_004_206);
    expect(() => buyPaymentZats(250_000_000, "4206")).toThrow(/six digits/);
  });
});

describe("parseOtpMemo", () => {
  const challenge = { code: "417293", name: "alice", action: "release" as const, ua: UA };

  it("reads back what buildOtpMemo writes, ignoring memo padding", () => {
    const text = buildOtpMemo(challenge);
    expect(parseOtpMemo(text)).toEqual(challenge);
    expect(parseOtpMemo(text + "\0".repeat(512 - text.length))).toEqual(challenge);
  });

  it("returns null for anything else", () => {
    expect(parseOtpMemo(`ZNS:claim:forever:alice:${UA}`)).toBeNull();
    expect(parseOtpMemo(`ZNS:otp:417293:alice:claim:${UA}`)).toBeNull();
    expect(parseOtpMemo(`ZNS:otp:417293:alice:update`)).toBeNull();
    expect(parseOtpMemo("hello world")).toBeNull();
  });
});

describe("buildZip321Uri", () => {
  it("builds a URI that parseZip321Uri reads back", () => {
    const memo = buildClaimMemo({ name: "alice", ua: UA, term: "forever" });
    const uri = buildZip321Uri({ address: TESTNET_UA, amount: "0.0123", memo });
    expect(uri).not.toMatch(/=$/);
    expect(parseZip321Uri(uri)).toEqual({
      address: TESTNET_UA,
      amount: "0.0123",
      memoRaw: uri.split("memo=")[1],
      memoDecoded: memo,
    });
  });

  it("rejects an invalid amount", () => {
    for (const amount of ["", "1.123456789", "-1", "1e3"]) {
      expect(() => buildZip321Uri({ address: TESTNET_UA, amount })).toThrow(/amount/);
    }
  });

  it("rejects an amount above 21,000,000 ZEC", () => {
    expect(buildZip321Uri({ address: TESTNET_UA, amount: "21000000" }))
      .toBe(`zcash:${TESTNET_UA}?amount=21000000`);
    expect(buildZip321Uri({ address: TESTNET_UA, amount: "21000000.00000000" }))
      .toBe(`zcash:${TESTNET_UA}?amount=21000000.00000000`);
    for (const amount of ["21000001", "21000000.00000001", "99999999999999999999"]) {
      expect(() => buildZip321Uri({ address: TESTNET_UA, amount })).toThrow(/21000000 ZEC/);
    }
  });

  it("rejects a memo longer than 512 UTF-8 bytes", () => {
    expect(() => buildZip321Uri({ address: TESTNET_UA, memo: "a".repeat(512) })).not.toThrow();
    expect(() => buildZip321Uri({ address: TESTNET_UA, memo: "a".repeat(513) })).toThrow(/512 bytes/);
    // 257 two-byte characters: 257 characters, 514 bytes.
    expect(() => buildZip321Uri({ address: TESTNET_UA, memo: "é".repeat(257) })).toThrow(/512 bytes/);
  });

  it("rejects a memo for a transparent address", () => {
    const taddr = "t1Hsc1LR8yKnbbe3twRp88p6vFfC5t7DLbs";
    expect(buildZip321Uri({ address: taddr, amount: "1" })).toBe(`zcash:${taddr}?amount=1`);
    expect(() => buildZip321Uri({ address: taddr, memo: "hi" })).toThrow(/transparent/);
  });

  it("rejects an address with a URI delimiter in it", () => {
    for (const address of ["", `${TESTNET_UA}#x`, `${TESTNET_UA}%20`, "a=b", "u1 x"]) {
      expect(() => buildZip321Uri({ address, amount: "1" })).toThrow(/address/);
    }
  });
});
