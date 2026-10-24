import { describe, it, expect } from "vitest";
import {
  ZNS,
  buildClaimMemo,
  buildUpdateMemo,
  buildReleaseMemo,
  buildOtpMemo,
  parseOtpMemo,
  buildZip321Uri,
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
    expect(() =>
      buildOtpMemo({ code: "417293", name: "alice", action: "claim" as never, ua: UA }),
    ).toThrow(/updates and releases/);
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
    expect(new ZNS().parseZip321Uri(uri)).toEqual({
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
});
