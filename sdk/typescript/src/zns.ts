import { bech32m } from "bech32";
import type {
  Network,
  Registration,
  Status,
  Event,
  EventsFilter,
  EventsResult,
} from "./types.js";

/** Network-specific configuration for ZNS. */
export const NETWORKS = {
  testnet: {
    url: "https://light.zcash.me/zns-testnet",
    registryAddress:
      "utest1f32kn6c4zvn54xr8wfsnxmj9hzpu2mwgtxzpzwcw34906tdccdvzs0z2dx38lly7tpan77x6udt8pjczqm22ymsdhlz9j0tk5yq664nl",
    uivk: "uivktest1hzw7wyadutvzfgpna80yftsk5l7jeyu2p5me5quvp28tytxueta00cx4068wnlzcv7tx9n3t3gfhsy83pe4y6jrhxtzaq0hj6xtg5zrk2dn7zen3vns2a5pgs4fxdjlletmqrhfa42",
  },
  mainnet: {
    url: "https://light.zcash.me/zns-mainnet",
    registryAddress:
      "u1k0evt0ahj5qdt6y9ftsxndl8lrkm4ff6rp00u04cjpmqj6hxl9t8hfsxftmn3ht34e03lljh89czn2h8qn67rwrs8x0hm3lsxsucp9q9",
    uivk: "uivk1gl26qy0xjja7lqhyg3pf0x4j4j66kqwewrjkdcg28eqq4wgtzjmujpee7x9cs2ec9xhnlgrm8ptlw8z80j2aryw8nqtssser2ys778a0s00uvgkdjnfr58sndhfvc3f4zqjs6ywva6",
  },
} as const;

/** Valid ZNS name pattern: 1-62 lowercase alphanumeric chars. */
const NAME_RE = /^[a-z0-9]{1,62}$/;

/** Validates a ZNS name format (lowercase alphanumeric, 1-62 chars). */
function isValidName(name: string): boolean {
  return NAME_RE.test(name);
}

/**
 * Normalizes a ZNS name for lookups: trims whitespace, lowercases, and
 * strips one trailing `.zcash` or `.zec` suffix (case-insensitive), so
 * `Alice.zcash`, `aLice.Zec`, and `alice` all normalize to `alice`.
 */
function normalizeName(name: string): string {
  return name
    .trim()
    .toLowerCase()
    .replace(/\.(zcash|zec)$/, "");
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

export class ZNS {
  private url: string;
  private network: Network;
  private rpcId = 0;
  private _verified = false;

  /**
   * Creates a new ZNS client.
   * @param options - Configuration options
   * @param options.network - Network to connect to ("testnet" | "mainnet"), defaults to "testnet"
   * @param options.url - Custom indexer URL (optional)
   */
  constructor(options?: { network?: Network; url?: string }) {
    this.network = options?.network ?? "testnet";
    this.url = options?.url ?? NETWORKS[this.network].url;
  }

  /**
   * Verifies that the connected server is a known ZNS instance.
   * @throws Error if the server's UIVK is not recognized
   */
  async verify(): Promise<void> {
    const status = await this.status();
    if (status.uivk !== NETWORKS[this.network].uivk) {
      throw new Error(
        `UIVK mismatch: indexer returned "${status.uivk.slice(0, 20)}..." which is not a known ZNS instance`,
      );
    }
    this._verified = true;
  }

  /** Returns true if {@link verify} has been called and passed. */
  get verified(): boolean {
    return this._verified;
  }

  /** Get the registry address for the current network. */
  get registryAddress(): string {
    return NETWORKS[this.network].registryAddress;
  }

  /** Fetch current server status. */
  async status(): Promise<Status> {
    const raw = await this.rpc<Record<string, unknown>>("status");
    return normalizeApiResponse<Status>(raw);
  }

  /** Resolve a ZNS name to its registration. Returns null if not registered.
   *
   *  The name is normalized before querying: trimmed, lowercased, and one
   *  trailing `.zcash`/`.zec` suffix stripped (case-insensitive), so
   *  `Alice.zcash`, `aLice.Zec`, and `alice` all resolve the same name.
   *  Returns null without hitting the server if the name is invalid after
   *  normalization. Note the returned registration's `name` field is the
   *  normalized form, not the raw input. */
  async resolveName(name: string): Promise<Registration | null> {
    const normalized = normalizeName(name);
    if (!isValidName(normalized)) return null;
    const raw = await this.rpc<Record<string, unknown> | null>("resolve", {
      query: normalized,
    });
    return raw ? normalizeApiResponse<Registration>(raw) : null;
  }

  /** Resolve a Zcash Unified Address to all names pointing to it. Returns empty array if none.
   *  Supports pagination with limit (default 50, max 500) and offset (default 0). */
  async resolveAddress(
    address: string,
    limit?: number,
    offset?: number,
  ): Promise<Registration[]> {
    const raw = await this.rpc<Record<string, unknown>[]>("resolve", {
      query: address,
      limit,
      offset,
    });
    return raw.map((r) => normalizeApiResponse<Registration>(r));
  }

  /** List all registered names. Useful for explorers or browsers.
   *  Supports pagination with limit (default 50, max 500) and offset (default 0). */
  async listAllRegistrations(
    limit?: number,
    offset?: number,
  ): Promise<Registration[]> {
    const raw = await this.rpc<Record<string, unknown>[]>("resolve", {
      query: "",
      limit,
      offset,
    });
    return raw.map((r) => normalizeApiResponse<Registration>(r));
  }

  /** Check if a name is available for registration.
   *  The name is normalized like {@link resolveName} first, so
   *  `isAvailable("Alice.zec")` checks "alice". Returns false immediately
   *  for names that are invalid after normalization, without hitting the
   *  server. */
  async isAvailable(name: string): Promise<boolean> {
    const normalized = normalizeName(name);
    if (!isValidName(normalized)) return false;
    const result = await this.resolveName(normalized);
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

  async events(filter?: EventsFilter): Promise<EventsResult> {
    const raw = await this.rpc<Record<string, unknown>>(
      "events",
      normalizeApiResponse(filter ?? {}) as Record<string, unknown>,
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
  ): Promise<T> {
    const id = ++this.rpcId;
    const body = JSON.stringify({ jsonrpc: "2.0", id, method, params });

    const res = await fetch(this.url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body,
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
  }
}
