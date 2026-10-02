import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { ZNS } from "../src/zns.js";

const VALID_TESTNET_UA = "utest100qlkeru5c3m5kfrwe2hsmcfzmusreaza2prdyelg2kd2tr2842nceq952vay3gpmgky09fgft4z57h4z2zqzz5rcwgd4q90u54ek5yyca4s6e6y2jja9sww27kzedzznjcupcu0svq2exvq995c0lhl5zm53g4ksnm2xuwt3snv4dgh";
const VALID_MAINNET_UA = "u1q8g0h9cn2x4eq8jd7k0d5y3zf6vhb5w4xj9tz3m5p6r2s1t0u7v8w9x0y1z";

describe("ZNS", () => {
  let zns: ZNS;

  beforeEach(() => {
    zns = new ZNS({ url: "https://resolver.example" });
  });

  describe("constructor", () => {
    it("uses the configured current resolver URL", () => {
      const z = new ZNS({ url: "https://resolver.example" });
      expect(z.url).toBe("https://resolver.example");
    });
  });

  describe("isValidName", () => {
    it("accepts valid names", () => {
      expect(zns.isValidName("alice")).toBe(true);
      expect(zns.isValidName("bob123")).toBe(true);
      expect(zns.isValidName("a")).toBe(true);
      expect(zns.isValidName("a".repeat(63))).toBe(true);
    });

    it("rejects invalid names", () => {
      expect(zns.isValidName("")).toBe(false);
      expect(zns.isValidName("Alice")).toBe(false);
      expect(zns.isValidName("my-name")).toBe(false);
      expect(zns.isValidName("a".repeat(64))).toBe(false);
    });
  });

  describe("normalizeName", () => {
    it("trims and lowercases lookup input", () => {
      expect(zns.normalizeName("alice")).toBe("alice");
      expect(zns.normalizeName("ALICE")).toBe("alice");
      expect(zns.normalizeName("  Alice  ")).toBe("alice");
    });

    it("does not strip suffixes or alter punctuation", () => {
      expect(zns.normalizeName("Alice.zcash")).toBe("alice.zcash");
      expect(zns.normalizeName("alice.eth")).toBe("alice.eth");
      expect(zns.normalizeName("alice.")).toBe("alice.");
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
                last_action: "claim",
                expires_at: "none",
              },
            }),
          };
        }),
      );
      return bodies;
    };

    it("sends the normalized name as the query", async () => {
      const bodies = stubResolve();
      for (const input of ["Alice", "alice", "aLiCe", "  ALICE  "]) {
        const reg = await zns.resolveName(input);
      expect(reg?.name).toBe("alice");
      expect(reg?.lastAction).toBe("claim");
      expect(reg?.expiresAt).toBe("none");
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
      expect(await zns.resolveName("alice.zec")).toBeNull();
      expect(await zns.resolveName("")).toBeNull();
      expect(await zns.isAvailable("alice.eth")).toBe(false);
      expect(fetchSpy).not.toHaveBeenCalled();
    });

    it("isAvailable checks the normalized name", async () => {
      const bodies = stubResolve();
      expect(await zns.isAvailable("Alice")).toBe(false);
      expect((bodies[0].params as { query: string }).query).toBe("alice");
    });
  });

  describe("status and events", () => {
    afterEach(() => {
      vi.unstubAllGlobals();
    });

    it("returns the current resolver status shape", async () => {
      vi.stubGlobal("fetch", vi.fn(async () => ({
        ok: true,
        json: async () => ({ result: {
          synced_height: 123,
          synced: true,
          viewing_key: "ufvk-test",
          registered: 7,
        } }),
      })));

      await expect(zns.status()).resolves.toEqual({
        syncedHeight: 123,
        synced: true,
        viewingKey: "ufvk-test",
        registered: 7,
      });
    });

    it("sends lowercase event filters using resolver parameter names", async () => {
      let request: Record<string, unknown> | undefined;
      vi.stubGlobal("fetch", vi.fn(async (_url: string, init: { body: string }) => {
        request = JSON.parse(init.body);
        return { ok: true, json: async () => ({ result: {
          events: [{
            id: 1,
            name: "alice",
            action: "update",
            txid: "ab".repeat(32),
            height: 123,
            action_index: 2,
            address: VALID_TESTNET_UA,
            expires_at: "none",
          }],
          total: 1,
          limit: 50,
          offset: 0,
        } }) };
      }));

      const result = await zns.events({ name: "alice", action: "update", sinceHeight: 100 });
      expect(request?.params).toEqual({ name: "alice", action: "update", since_height: 100 });
      expect(result.events[0]).toMatchObject({
        action: "update",
        actionIndex: 2,
        address: VALID_TESTNET_UA,
        expiresAt: "none",
      });
      expect(result).toMatchObject({ total: 1, limit: 50, offset: 0 });
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
