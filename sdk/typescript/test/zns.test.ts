import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { ZNS, ZNS_NETWORKS } from "../src/zns.js";

const VALID_TESTNET_UA = "utest100qlkeru5c3m5kfrwe2hsmcfzmusreaza2prdyelg2kd2tr2842nceq952vay3gpmgky09fgft4z57h4z2zqzz5rcwgd4q90u54ek5yyca4s6e6y2jja9sww27kzedzznjcupcu0svq2exvq995c0lhl5zm53g4ksnm2xuwt3snv4dgh";
const VALID_MAINNET_UA = "u1f68wxdxsrswjl04kcc397a9ljedg88rl8dgazrk8l9v8jx6t3gqsktvqyvmc3ypgf4pum03q4ftvy5s0fe803nrxm8kw6dgzlvnv83nv";

/** A registration as the resolver sends it. */
const record = (name: string, address = VALID_TESTNET_UA, lastAction = "claim") => ({
  name,
  address,
  txid: "ab".repeat(32),
  height: 100,
  last_action: lastAction,
  expires_at: "none",
});

/**
 * A resolver that serves the full list (`resolve` with no params) and
 * per-name `resolve`. Every request body is kept in `bodies`.
 */
function stubResolver(names: Array<Record<string, unknown>> = []) {
  const state = { names, listFails: false };
  const bodies: Array<{ method: string; params: Record<string, unknown> }> = [];
  vi.stubGlobal("fetch", vi.fn(async (_url: string, init: { body: string }) => {
    const body = JSON.parse(init.body);
    bodies.push(body);
    if (Object.keys(body.params).length === 0 && state.listFails) {
      throw new TypeError("fetch failed");
    }
    const result = "query" in body.params
      ? state.names.find((n) => n.name === body.params.query) ?? null
      : state.names;
    return { ok: true, json: async () => ({ result }) };
  }));
  return { state, bodies };
}

const isNameRequest = (body: { params: Record<string, unknown> }) => "query" in body.params;

describe("ZNS", () => {
  let zns: ZNS;

  beforeEach(() => {
    // `new ZNS()` downloads the name list, so fetch is always stubbed.
    stubResolver();
    zns = new ZNS({ url: "https://resolver.example" });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.useRealTimers();
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

    it("lowercases ASCII letters only", () => {
      expect(zns.normalizeName("\u212A")).toBe("\u212A");
      expect(zns.isValidName(zns.normalizeName("\u212A"))).toBe(false);
    });
  });

  describe("resolveName", () => {
    it("never sends the name it looks up", async () => {
      const { bodies } = stubResolver([record("alice"), record("bob")]);
      const z = new ZNS({ url: "https://resolver.example" });
      expect((await z.resolveName("alice"))?.name).toBe("alice");
      expect((await z.resolveName("  Bob  "))?.name).toBe("bob");
      expect(await z.resolveName("carol")).toBeNull();
      expect(await z.isAvailable("Alice")).toBe(false);
      expect(await z.isAvailable("carol")).toBe(true);

      expect(bodies).toEqual([{ jsonrpc: "2.0", id: 1, method: "resolve", params: {} }]);
    });

    it("returns null for invalid names without waiting for the list", async () => {
      // The list never arrives.
      vi.stubGlobal("fetch", vi.fn(() => new Promise(() => {})));
      const z = new ZNS({ url: "https://resolver.example" });
      expect(await z.resolveName("alice.eth")).toBeNull();
      expect(await z.resolveName("alice.zec")).toBeNull();
      expect(await z.resolveName("")).toBeNull();
      expect(await z.isAvailable("alice.eth")).toBe(false);
    });

    it("reads a sale, list, auction, and delist", async () => {
      const actions = ["sale", "list", "auction", "delist"];
      stubResolver(actions.map((action) => record(action, VALID_TESTNET_UA, action)));
      const z = new ZNS({ url: "https://resolver.example" });
      for (const action of actions) {
        expect((await z.resolveName(action))?.lastAction).toBe(action);
      }
    });

    it("refuses a list with an action the resolver does not send", async () => {
      stubResolver([record("alice", VALID_TESTNET_UA, "buy")]);
      const z = new ZNS({ url: "https://resolver.example" });
      await expect(z.resolveName("alice")).rejects.toThrow(/could not be downloaded/);
    });

    it("downloads the list again every 30 seconds", async () => {
      vi.useFakeTimers();
      const { state, bodies } = stubResolver([record("alice")]);
      const z = new ZNS({ url: "https://resolver.example" });
      expect((await z.resolveName("alice"))?.address).toBe(VALID_TESTNET_UA);

      state.names = [record("alice", VALID_MAINNET_UA, "update")];
      await vi.advanceTimersByTimeAsync(30_000);
      expect((await z.resolveName("alice"))?.address).toBe(VALID_MAINNET_UA);
      expect(bodies).toHaveLength(2);
      expect(bodies.filter(isNameRequest)).toEqual([]);
    });

    it("throws after a failed download, and never asks for the name", async () => {
      vi.useFakeTimers();
      const { state, bodies } = stubResolver([record("alice")]);
      const z = new ZNS({ url: "https://resolver.example" });
      expect((await z.resolveName("alice"))?.name).toBe("alice");

      state.listFails = true;
      await vi.advanceTimersByTimeAsync(30_000);
      await expect(z.resolveName("alice")).rejects.toThrow(/could not be downloaded/);
      expect(bodies.filter(isNameRequest)).toEqual([]);

      state.listFails = false;
      await vi.advanceTimersByTimeAsync(30_000);
      expect((await z.resolveName("alice"))?.name).toBe("alice");
    });
  });

  describe("resolveNameOnServer", () => {
    it("sends the normalized name to the resolver", async () => {
      const { bodies } = stubResolver([record("alice")]);
      const z = new ZNS({ url: "https://resolver.example" });
      expect((await z.resolveNameOnServer("  Alice "))?.name).toBe("alice");
      expect(bodies.at(-1)).toMatchObject({ method: "resolve", params: { query: "alice" } });
      expect(await z.resolveNameOnServer("bob")).toBeNull();
    });

    it("returns null for invalid names without sending them", async () => {
      const { bodies } = stubResolver();
      const z = new ZNS({ url: "https://resolver.example" });
      expect(await z.resolveNameOnServer("alice.eth")).toBeNull();
      expect(bodies.filter(isNameRequest)).toEqual([]);
    });
  });

  describe("status and events", () => {
    afterEach(() => {
      vi.unstubAllGlobals();
    });

    it("sends normalized event filters using resolver parameter names", async () => {
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

      const result = await zns.events({ name: " Alice ", action: "update", sinceHeight: 100 });
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

  describe("network presets", () => {
    it("defaults to the testnet preset (fail safe)", () => {
      const z = new ZNS();
      expect(z.network).toBe("testnet");
      expect(z.url).toBe("https://light.zcash.me/zns-testnet");
    });

    it("selects the mainnet preset by network", () => {
      const z = new ZNS({ network: "mainnet" });
      expect(z.network).toBe("mainnet");
      expect(z.url).toBe("https://main.zcashnames.com");
    });

    it("an explicit url always wins over the preset", () => {
      const z = new ZNS({ network: "mainnet", url: "https://resolver.example" });
      expect(z.network).toBe("mainnet");
      expect(z.url).toBe("https://resolver.example");
    });
  });

  describe("verify", () => {
    afterEach(() => {
      vi.unstubAllGlobals();
    });

    const stubStatus = (body: Record<string, unknown>) => {
      let request: Record<string, unknown> | undefined;
      vi.stubGlobal(
        "fetch",
        vi.fn(async (_url: string, init: { body: string }) => {
          request = JSON.parse(init.body);
          return { ok: true, json: async () => ({ result: body }) };
        }),
      );
      return () => request;
    };

    it("passes when the server serves the baked testnet viewing key", async () => {
      const z = new ZNS({ network: "testnet", url: "https://resolver.example" });
      stubStatus({
        synced_height: 1,
        viewing_key: ZNS_NETWORKS.testnet.viewingKey,
      });
      await expect(z.verify()).resolves.toBeUndefined();
    });

    it("passes when the server serves the baked mainnet viewing key", async () => {
      const z = new ZNS({ network: "mainnet", url: "https://resolver.example" });
      stubStatus({
        synced_height: 1,
        viewing_key: ZNS_NETWORKS.mainnet.viewingKey,
      });
      await expect(z.verify()).resolves.toBeUndefined();
    });

    it("sends a status request", async () => {
      const z = new ZNS({ network: "testnet", url: "https://resolver.example" });
      const getRequest = stubStatus({ viewing_key: ZNS_NETWORKS.testnet.viewingKey });
      await z.verify();
      expect(getRequest()?.method).toBe("status");
    });

    it("reports the server's key through the single viewingKey field", async () => {
      const z = new ZNS({ network: "mainnet", url: "https://resolver.example" });
      vi.stubGlobal("fetch", vi.fn(async () => ({
        ok: true,
        json: async () => ({ result: {
          synced_height: 3516184,
          viewing_key: ZNS_NETWORKS.mainnet.viewingKey,
        } }),
      })));

      const status = await z.status();
      expect(status.viewingKey).toBe(ZNS_NETWORKS.mainnet.viewingKey);
    });

    it("reports the treasury address through the treasuryAddress field", async () => {
      const z = new ZNS({ network: "testnet", url: "https://resolver.example" });
      vi.stubGlobal("fetch", vi.fn(async () => ({
        ok: true,
        json: async () => ({ result: {
          synced_height: 4529119,
          viewing_key: ZNS_NETWORKS.testnet.viewingKey,
          treasury_address: ZNS_NETWORKS.testnet.treasuryAddress,
        } }),
      })));

      const status = await z.status();
      expect(status.treasuryAddress).toBe(ZNS_NETWORKS.testnet.treasuryAddress);
    });

    it("throws with upgrade guidance on a key mismatch", async () => {
      const z = new ZNS({ network: "testnet", url: "https://resolver.example" });
      stubStatus({ synced_height: 1, viewing_key: "uviewtest1rotated" });
      await expect(z.verify()).rejects.toThrow(/registry rotation\. Upgrade zcashname-sdk/);
    });
  });

  describe("reverseLookup and listNames", () => {
    afterEach(() => {
      vi.unstubAllGlobals();
    });

    it("calls the typed reverse_lookup verb and returns the total", async () => {
      let request: Record<string, unknown> | undefined;
      vi.stubGlobal("fetch", vi.fn(async (_url: string, init: { body: string }) => {
        request = JSON.parse(init.body);
        return { ok: true, json: async () => ({ result: {
          items: [{
            name: "alice",
            address: VALID_TESTNET_UA,
            txid: "ab".repeat(32),
            height: 100,
            last_action: "claim",
            expires_at: "none",
          }],
          total: 3,
          limit: 1,
          offset: 0,
        } }) };
      }));

      const page = await zns.reverseLookup(VALID_TESTNET_UA, 1, 0);
      expect(request?.method).toBe("reverse_lookup");
      expect(request?.params).toEqual({ address: VALID_TESTNET_UA, limit: 1, offset: 0 });
      expect(page.total).toBe(3);
      expect(page.items[0]).toMatchObject({ name: "alice", lastAction: "claim" });

      // resolveAddress is the same lookup, its items only.
      const items = await zns.resolveAddress(VALID_TESTNET_UA, 1, 0);
      expect(request?.method).toBe("reverse_lookup");
      expect(items).toEqual(page.items);
    });

    it("calls the typed list_names verb", async () => {
      let request: Record<string, unknown> | undefined;
      vi.stubGlobal("fetch", vi.fn(async (_url: string, init: { body: string }) => {
        request = JSON.parse(init.body);
        return { ok: true, json: async () => ({ result: { items: [], total: 0, limit: 50, offset: 0 } }) };
      }));

      const page = await zns.listNames(50, 10);
      expect(request?.method).toBe("list_names");
      expect(request?.params).toEqual({ limit: 50, offset: 10 });
      expect(page.items).toEqual([]);
    });
  });

  describe("timeout", () => {
    afterEach(() => {
      vi.unstubAllGlobals();
      vi.useRealTimers();
    });

    it("aborts an in-flight request after the configured timeout", async () => {
      vi.useFakeTimers();
      const z = new ZNS({ url: "https://resolver.example", timeout: 50 });
      vi.stubGlobal(
        "fetch",
        vi.fn(
          (_url: string, init: { signal: AbortSignal }) =>
            new Promise((_resolve, reject) => {
              init.signal.addEventListener("abort", () =>
                reject(new Error("The operation was aborted")),
              );
            }),
        ),
      );
      const pending = z.status();
      const assertion = expect(pending).rejects.toThrow();
      await vi.advanceTimersByTimeAsync(60);
      await assertion;
    });

    it("rejects when the caller's signal is already aborted", async () => {
      const z = new ZNS({ url: "https://resolver.example" });
      const controller = new AbortController();
      controller.abort();
      vi.stubGlobal("fetch", vi.fn(async (
        _url: string,
        init: { signal: AbortSignal },
      ) => {
        if (init.signal.aborted) throw new Error("This operation was aborted");
        return { ok: true, json: async () => ({ result: {} }) };
      }));

      await expect(z.status(controller.signal)).rejects.toThrow(/aborted/);
    });
  });

  describe("endpoint fallback", () => {
    const okStatus = { result: { synced_height: 1, viewing_key: "uview-x" } };

    afterEach(() => {
      vi.unstubAllGlobals();
    });

    it("uses the mainnet preset's endpoints in order", () => {
      const z = new ZNS({ network: "mainnet" });
      expect(z.url).toBe("https://main.zcashnames.com");
    });

    it("falls back to the next endpoint when one is unreachable", async () => {
      const fetchMock = vi.fn(async (url: string) => {
        if (url === "https://main.zcashnames.com") throw new TypeError("fetch failed");
        return { ok: true, json: async () => okStatus };
      });
      const z = new ZNS({ network: "mainnet" });
      vi.stubGlobal("fetch", fetchMock);

      const status = await z.status();
      expect(status.syncedHeight).toBe(1);
      expect(fetchMock.mock.calls.map(([url]) => url)).toEqual([
        "https://main.zcashnames.com",
        "https://light.zcash.me/zns-mainnet",
      ]);
    });

    it("falls back on a server error", async () => {
      const fetchMock = vi.fn(async (url: string) => {
        if (url === "https://main.zcashnames.com") {
          return { ok: false, status: 503, statusText: "Unavailable" };
        }
        return { ok: true, json: async () => okStatus };
      });
      const z = new ZNS({ network: "mainnet" });
      vi.stubGlobal("fetch", fetchMock);

      await expect(z.status()).resolves.toMatchObject({
        syncedHeight: 1,
      });
      expect(fetchMock).toHaveBeenCalledTimes(2);
    });

    it("does not fall back when the resolver answers with an RPC error", async () => {
      const fetchMock = vi.fn(async () => ({
        ok: true,
        json: async () => ({ error: { code: -1, message: "nope" } }),
      }));
      const z = new ZNS({ network: "mainnet" });
      vi.stubGlobal("fetch", fetchMock);

      await expect(z.status()).rejects.toThrow(/nope/);
      expect(fetchMock).toHaveBeenCalledTimes(1);
    });

    it("does not fall back on a client error", async () => {
      const fetchMock = vi.fn(async () => ({
        ok: false,
        status: 404,
        statusText: "Not Found",
      }));
      const z = new ZNS({ network: "mainnet" });
      vi.stubGlobal("fetch", fetchMock);

      await expect(z.status()).rejects.toThrow(/404/);
      expect(fetchMock).toHaveBeenCalledTimes(1);
    });

    it("throws the last failure when every endpoint is unreachable", async () => {
      const fetchMock = vi.fn(async () => {
        throw new TypeError("fetch failed");
      });
      const z = new ZNS({ network: "mainnet" });
      vi.stubGlobal("fetch", fetchMock);

      await expect(z.status()).rejects.toThrow(/fetch failed/);
      expect(fetchMock).toHaveBeenCalledTimes(2);
    });

    it("does not fall back when an explicit url is set", async () => {
      const fetchMock = vi.fn(async () => {
        throw new TypeError("fetch failed");
      });
      const z = new ZNS({ network: "mainnet", url: "https://resolver.example" });
      vi.stubGlobal("fetch", fetchMock);

      await expect(z.status()).rejects.toThrow(/fetch failed/);
      expect(fetchMock).toHaveBeenCalledTimes(1);
    });
  });
});
