import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { ZNS } from "../src/zns.js";

const VALID_TESTNET_UA = "utest100qlkeru5c3m5kfrwe2hsmcfzmusreaza2prdyelg2kd2tr2842nceq952vay3gpmgky09fgft4z57h4z2zqzz5rcwgd4q90u54ek5yyca4s6e6y2jja9sww27kzedzznjcupcu0svq2exvq995c0lhl5zm53g4ksnm2xuwt3snv4dgh";
const VALID_MAINNET_UA = "u1q8g0h9cn2x4eq8jd7k0d5y3zf6vhb5w4xj9tz3m5p6r2s1t0u7v8w9x0y1z";

describe("ZNS", () => {
  let zns: ZNS;

  beforeEach(() => {
    zns = new ZNS();
  });

  describe("constructor", () => {
    it("defaults to testnet", () => {
      const z = new ZNS();
      expect(z.network).toBe("testnet");
    });

    it("accepts network option", () => {
      const z = new ZNS({ network: "mainnet" });
      expect(z.network).toBe("mainnet");
    });

    it("accepts custom url", () => {
      const z = new ZNS({ url: "https://custom.example.com" });
      expect(z.url).toBe("https://custom.example.com");
    });
  });

  describe("isValidName", () => {
    it("accepts valid names", () => {
      expect(zns.isValidName("alice")).toBe(true);
      expect(zns.isValidName("bob123")).toBe(true);
      expect(zns.isValidName("a")).toBe(true);
      expect(zns.isValidName("a".repeat(62))).toBe(true);
    });

    it("rejects invalid names", () => {
      expect(zns.isValidName("")).toBe(false);
      expect(zns.isValidName("Alice")).toBe(false);
      expect(zns.isValidName("my-name")).toBe(false);
      expect(zns.isValidName("a".repeat(63))).toBe(false);
    });
  });

  describe("normalizeName", () => {
    it("lowercases and strips .zcash/.zec suffix in any case", () => {
      expect(zns.normalizeName("alice")).toBe("alice");
      expect(zns.normalizeName("Alice.zcash")).toBe("alice");
      expect(zns.normalizeName("alice.zec")).toBe("alice");
      expect(zns.normalizeName("aLice.Zec")).toBe("alice");
      expect(zns.normalizeName("alice.ZCASH")).toBe("alice");
      expect(zns.normalizeName("ALICE")).toBe("alice");
      expect(zns.normalizeName("  alice.Zec  ")).toBe("alice");
    });

    it("does not strip non-suffix lookalikes", () => {
      expect(zns.normalizeName("zec")).toBe("zec");
      expect(zns.normalizeName("zcash")).toBe("zcash");
      expect(zns.normalizeName("alice.eth")).toBe("alice.eth");
      expect(zns.normalizeName("alice.")).toBe("alice.");
    });

    it("strips only one suffix", () => {
      expect(zns.normalizeName("alice.zec.zec")).toBe("alice.zec");
      expect(zns.normalizeName(".zec")).toBe("");
    });
  });

  describe("resolveName normalization", () => {
    afterEach(() => {
      vi.unstubAllGlobals();
    });

    const stubResolve = () => {
      const bodies: Array<Record<string, unknown>> = [];
      vi.stubGlobal(
        "fetch",
        vi.fn(async (_url: string, init: { body: string }) => {
          const body = JSON.parse(init.body);
          bodies.push(body);
          return {
            ok: true,
            json: async () => ({
              result: {
                name: "alice",
                address: VALID_TESTNET_UA,
                txid: "ab".repeat(32),
                height: 100,
                nonce: 0,
                last_action: "CLAIM",
              },
            }),
          };
        }),
      );
      return bodies;
    };

    it("sends the normalized name as the query", async () => {
      const bodies = stubResolve();
      for (const input of ["Alice.zcash", "alice.zec", "aLice.Zec", "alice.ZCASH"]) {
        const reg = await zns.resolveName(input);
        expect(reg?.name).toBe("alice");
      }
      expect(bodies.map((b) => (b.params as { query: string }).query)).toEqual([
        "alice",
        "alice",
        "alice",
        "alice",
      ]);
    });

    it("returns null for invalid names without hitting the server", async () => {
      const fetchSpy = vi.fn();
      vi.stubGlobal("fetch", fetchSpy);
      expect(await zns.resolveName("alice.eth")).toBeNull();
      expect(await zns.resolveName("alice.zec.zec")).toBeNull();
      expect(await zns.resolveName(".zec")).toBeNull();
      expect(await zns.resolveName("")).toBeNull();
      expect(await zns.isAvailable("alice.eth")).toBe(false);
      expect(fetchSpy).not.toHaveBeenCalled();
    });

    it("isAvailable checks the normalized name", async () => {
      const bodies = stubResolve();
      expect(await zns.isAvailable("Alice.zec")).toBe(false);
      expect((bodies[0].params as { query: string }).query).toBe("alice");
    });
  });

  describe("isValidUnifiedAddress", () => {
    it("accepts valid testnet UA", () => {
      expect(zns.isValidUnifiedAddress(VALID_TESTNET_UA)).toBe(true);
    });

    it("accepts valid mainnet UA", () => {
      expect(zns.isValidUnifiedAddress(VALID_MAINNET_UA)).toBe(true);
    });

    it("rejects invalid addresses", () => {
      expect(zns.isValidUnifiedAddress("")).toBe(false);
      expect(zns.isValidUnifiedAddress("notanaddress")).toBe(false);
      expect(zns.isValidUnifiedAddress("zs1abc...")).toBe(false);
    });
  });

  describe("parseZip321Uri", () => {
    it("parses address and amount", () => {
      const result = zns.parseZip321Uri("zcash:u1abc?amount=1.5&memo=abc123");
      expect(result.address).toBe("u1abc");
      expect(result.amount).toBe("1.5");
    });

    it("decodes base64url memo", () => {
      const result = zns.parseZip321Uri("zcash:u1abc?memo=SGVsbG8");
      expect(result.memoDecoded).toBe("Hello");
    });

    it("returns empty strings for missing fields", () => {
      const result = zns.parseZip321Uri("zcash:u1abc");
      expect(result.amount).toBe("");
      expect(result.memoRaw).toBe("");
      expect(result.memoDecoded).toBe("");
    });

    it("handles zcash: prefix case-insensitively", () => {
      const result = zns.parseZip321Uri("ZCASH:u1abc");
      expect(result.address).toBe("u1abc");
    });
  });

});
