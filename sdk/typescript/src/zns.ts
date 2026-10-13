import { bech32m } from "bech32";
import type {
  Registration,
  Status,
  Event,
  EventsFilter,
  EventsResult,
  Paginated,
} from "./types.js";

export * from "./pricing.js";

/** Valid ZNS name pattern: 1-63 lowercase alphanumeric chars. */
const NAME_RE = /^[a-z0-9]{1,63}$/;

/** Validates a ZNS name format (lowercase alphanumeric, 1-63 chars). */
function isValidName(name: string): boolean {
  return NAME_RE.test(name);
}

/**
 * Canonicalizes lookup input by trimming whitespace and lowercasing.
 */
function normalizeName(name: string): string {
  return name.trim().toLowerCase();
}

/**
 * Normalizes indexer API responses from snake_case (Rust) to camelCase (TypeScript).
 */
function normalizeApiResponse<T>(obj: unknown): T {
  if (Array.isArray(obj))
    return obj.map((item) => normalizeApiResponse(item)) as T;
  if (obj && typeof obj === "object") {
    return Object.fromEntries(
      Object.entries(obj).map(([k, v]) => [
        k.replace(/_([a-z])/g, (_, c) => c.toUpperCase()),
        normalizeApiResponse(v),
      ]),
    ) as T;
  }
  return obj as T;
}

function toRpcParams(obj: Record<string, unknown>): Record<string, unknown> {
  return Object.fromEntries(
    Object.entries(obj)
      .filter(([, value]) => value !== undefined)
      .map(([key, value]) => [key.replace(/[A-Z]/g, (c) => `_${c.toLowerCase()}`), value]),
  );
}

/** Validates a Zcash unified (u-) address. */
function isValidUnifiedAddress(address: string): boolean {
  if (!address) return false;
  if (address.startsWith("utest1")) return true;
  if (address.startsWith("u1")) return true;
  try {
    const decoded = bech32m.decode(address);
    return decoded.prefix === "u" || decoded.prefix === "utest";
  } catch {
    return false;
  }
}

/** The ZNS networks. */
export type Network = "mainnet" | "testnet";

/**
 * ⚠️⚠️⚠️  PLACEHOLDER REGISTRY KEYS — NOT THE FINAL SDK UFVKs  ⚠️⚠️⚠️
 *
 * The `viewingKey` entries below are exactly what the LIVE resolvers serve
 * in `status` today, and exist only so `verify()` works against current
 * deployments:
 *
 *   - testnet: the current (Name Note) resolver, v0.1.9 ceremony era;
 *   - mainnet: the LEGACY indexer, still serving its UIVK-era key.
 *
 * A future ceremony will publish the final registry keys (the planned
 * mainnet UFVK starts `uview1hzzcqccht7226...` and takes over once mainnet
 * runs the current resolver). When that happens BOTH entries are replaced
 * and every 0.14.0-rc.* release is superseded.
 *
 * Do not pin 0.14.0-rc.* anywhere durable. Do not build tooling around
 * these values.
 */
export const ZNS_NETWORKS = {
  testnet: {
    url: "https://light.zcash.me/zns-testnet",
    viewingKey:
      "uviewtest19e96vt6h4mv37pcg8w6fqckrpf9ku24fmwhpys30l76zs2fke2ns34nw9725an3w3n5kl0gc8hf6w6rdr4xmdhmj0p8t3gt3ynj8a4e3csy3l3df4rre9cljq9w4424pn3az4lvqpgkl37vfc3qf8qu23tcjlrwjayanam2wynrnqpejcly9eccy2nknukrkml7fsgp0gzz55nwml7ary22240r5m8737n206vnpdxxkdhl3pmxpg70d0tdckfwrthqja6dcy88gdur2k5m6qzpvuqddsxhwytey4fefukll0sh63fpd64gxwawm2pasas0du4c4v6k2ql2zf80qfsfcr4p99a03kn2a25q8vknye9ywcagss7vwhq383cs60469wm5repjv4z3rc2g69xyfrn4anxljmjywxdh32gvay3sl3xpa9m6al3xfxmjylcygpgdm47yrh7vtqnmp0zfe60k6uuc0mxt62ntc5r6a4czfsg4cae0h",
  },
  mainnet: {
    url: "https://light.zcash.me/zns-mainnet",
    viewingKey:
      "uivk1gl26qy0xjja7lqhyg3pf0x4j4j66kqwewrjkdcg28eqq4wgtzjmujpee7x9cs2ec9xhnlgrm8ptlw8z80j2aryw8nqtssser2ys778a0s00uvgkdjnfr58sndhfvc3f4zqjs6ywva6",
  },
} as const satisfies Record<Network, { url: string; viewingKey: string }>;

export class ZNS {
  /** The resolver JSON-RPC endpoint in use. */
  url: string;
  /** The network preset in use. */
  network: Network;
  private rpcId = 0;
  private timeoutMs?: number;

  /**
   * Creates a new ZNS client.
   * @param options.network - Network preset, selects the default endpoint
   *   (defaults to "testnet" — fail safe)
   * @param options.url - Explicit resolver JSON-RPC endpoint; always wins
   *   over the network preset
   * @param options.timeout - Abort in-flight requests after this many
   *   milliseconds
   */
  constructor(options: { network?: Network; url?: string; timeout?: number } = {}) {
    this.network = options.network ?? "testnet";
    this.url = options.url ?? ZNS_NETWORKS[this.network].url;
    this.timeoutMs = options.timeout;
  }

  /** Fetch current server status. */
  async status(signal?: AbortSignal): Promise<Status> {
    const raw = await this.rpc<Record<string, unknown>>("status", {}, signal);
    return normalizeApiResponse<Status>(raw);
  }

  /**
   * Verifies this client is talking to the ZNS registry for its network by
   * comparing the server's served viewing key against the key baked into
   * this SDK. Call once at startup; on failure do not trust the server.
   *
   * @throws Error naming both possible causes: the endpoint is not the ZNS
   *   resolver, or this SDK predates a registry rotation.
   */
  async verify(signal?: AbortSignal): Promise<void> {
    const expected = ZNS_NETWORKS[this.network].viewingKey;
    if (!expected) {
      throw new Error(
        `ZNS: no registry viewing key is published for ${this.network} yet`,
      );
    }
    const status = await this.status(signal);
    if (status.viewingKey !== expected) {
      throw new Error(
        `ZNS: the server's viewing key does not match the ZNS ${this.network} registry — either this is not the ZNS resolver, or this SDK predates a registry rotation. Upgrade zcashname-sdk.`,
      );
    }
  }

  /** Resolve a ZNS name to its registration. Returns null if not registered.
   *
   *  The name is trimmed and lowercased before querying, so `Alice` and
   *  `alice` resolve the same name.
   *  Returns null without hitting the server if the name is invalid after
   *  normalization. Note the returned registration's `name` field is the
   *  normalized form, not the raw input. */
  async resolveName(
    name: string,
    signal?: AbortSignal,
  ): Promise<Registration | null> {
    const normalized = normalizeName(name);
    if (!isValidName(normalized)) return null;
    const raw = await this.rpc<Record<string, unknown> | null>(
      "resolve",
      { query: normalized },
      signal,
    );
    return raw ? normalizeApiResponse<Registration>(raw) : null;
  }

  /** Resolve a Zcash Unified Address to all names pointing to it. Returns empty array if none.
   *  Supports pagination with limit (default 50, max 500) and offset (default 0). */
  async resolveAddress(
    address: string,
    limit?: number,
    offset?: number,
    signal?: AbortSignal,
  ): Promise<Registration[]> {
    const raw = await this.rpc<Record<string, unknown>[]>(
      "resolve",
      { query: address, limit, offset },
      signal,
    );
    return raw.map((r) => normalizeApiResponse<Registration>(r));
  }

  /** Reverse lookup: all names currently bound to a unified address, with
   *  the total match count. Uses the resolver's typed `reverse_lookup` verb.
   *  Requires the current (Name Note) resolver — the legacy mainnet indexer
   *  does not implement this method yet. */
  async reverseLookup(
    address: string,
    limit?: number,
    offset?: number,
    signal?: AbortSignal,
  ): Promise<Paginated<Registration>> {
    const raw = await this.rpc<Record<string, unknown>>(
      "reverse_lookup",
      toRpcParams({ address, limit, offset }),
      signal,
    );
    return normalizeApiResponse<Paginated<Registration>>(raw);
  }

  /** List all registered names. Useful for explorers or browsers.
   *  Supports pagination with limit (default 50, max 500) and offset (default 0). */
  async listAllRegistrations(
    limit?: number,
    offset?: number,
    signal?: AbortSignal,
  ): Promise<Registration[]> {
    const raw = await this.rpc<Record<string, unknown>[]>(
      "resolve",
      { query: "", limit, offset },
      signal,
    );
    return raw.map((r) => normalizeApiResponse<Registration>(r));
  }

  /** List all currently registered names via the typed `list_names` verb,
   *  with the total match count. Requires the current (Name Note) resolver
   *  — the legacy mainnet indexer does not implement this method yet. */
  async listNames(
    limit?: number,
    offset?: number,
    signal?: AbortSignal,
  ): Promise<Paginated<Registration>> {
    const raw = await this.rpc<Record<string, unknown>>(
      "list_names",
      toRpcParams({ limit, offset }),
      signal,
    );
    return normalizeApiResponse<Paginated<Registration>>(raw);
  }

  /** Check if a name is available for registration.
   *  The name is normalized like {@link resolveName} first, so
   *  `isAvailable("Alice")` checks "alice". Returns false immediately
   *  for names that are invalid after normalization, without hitting the
   *  server. */
  async isAvailable(name: string, signal?: AbortSignal): Promise<boolean> {
    const normalized = normalizeName(name);
    if (!isValidName(normalized)) return false;
    const result = await this.resolveName(normalized, signal);
    return result === null;
  }

  /** Validate a Zcash Unified Address format.
   *  Accepts both mainnet ('u') and testnet ('utest') prefixes.
   *  Performs basic format validation but NOT full bech32m checksum verification.
   *  Returns true if the address looks like a unified address, false otherwise.
   *
   *  @todo(F4Jumble) Upgrade to full ZIP-316 decoding with F4Jumble to:
   *    - Parse actual typecodes from address items
   *    - Validate F4Jumble checksum (not just bech32m)
   *    - Optionally enforce: address must contain at least one Orchard receiver (typecode 0x03) */
  isValidName = isValidName;
  normalizeName = normalizeName;
  isValidUnifiedAddress = isValidUnifiedAddress;

  async events(
    filter?: EventsFilter,
    signal?: AbortSignal,
  ): Promise<EventsResult> {
    const raw = await this.rpc<Record<string, unknown>>(
      "events",
      toRpcParams((filter ?? {}) as Record<string, unknown>),
      signal,
    );
    return normalizeApiResponse<EventsResult>(raw);
  }

  /** Parse a ZIP-321 URI into its components. */
  parseZip321Uri(uri: string): {
    address: string;
    amount: string;
    memoRaw: string;
    memoDecoded: string;
  } {
    const withoutScheme = String(uri ?? "").replace(/^zcash:/i, "");
    const [addressPart, queryPart = ""] = withoutScheme.split("?");
    const address = addressPart.trim();
    const params = new URLSearchParams(queryPart);
    const amount = String(params.get("amount") ?? "").trim();
    const memoRaw = String(params.get("memo") ?? "").trim();
    const memoDecoded = memoRaw ? this.decodeBase64Url(memoRaw) : "";
    return { address, amount, memoRaw, memoDecoded };
  }

  // ── Private helpers ────────────────────────────────────────────────────────

  private decodeBase64Url(value: string): string {
    try {
      const normalized = String(value).replace(/-/g, "+").replace(/_/g, "/");
      const padLen =
        normalized.length % 4 === 0 ? 0 : 4 - (normalized.length % 4);
      const padded = normalized + "=".repeat(padLen);
      const binary = atob(padded);
      const bytes = Uint8Array.from(binary, (c) => c.charCodeAt(0));
      return new TextDecoder().decode(bytes);
    } catch {
      return "";
    }
  }

  private async rpc<T>(
    method: string,
    params: Record<string, unknown> = {},
    signal?: AbortSignal,
  ): Promise<T> {
    const id = ++this.rpcId;
    const body = JSON.stringify({ jsonrpc: "2.0", id, method, params });

    // Combine the caller's optional signal with the configured timeout into
    // one controller so either cancels the in-flight request.
    const controller = new AbortController();
    const timer =
      this.timeoutMs != null
        ? setTimeout(() => controller.abort(), this.timeoutMs)
        : null;
    const forward = () => controller.abort();
    if (signal?.aborted) controller.abort();
    else signal?.addEventListener("abort", forward);
    try {
      const res = await fetch(this.url, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body,
        signal: controller.signal,
      });

      if (!res.ok) {
        throw new Error(`ZNS HTTP ${res.status}: ${res.statusText}`);
      }

      const json = (await res.json()) as {
        result?: T;
        error?: { code: number; message: string };
      };

      if (json.error) {
        throw new Error(
          `ZNS RPC error ${json.error.code}: ${json.error.message}`,
        );
      }

      return json.result as T;
    } finally {
      if (timer != null) clearTimeout(timer);
      signal?.removeEventListener("abort", forward);
    }
  }
}
