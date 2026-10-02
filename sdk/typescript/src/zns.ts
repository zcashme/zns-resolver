import { bech32m } from "bech32";
import type {
  Registration,
  Status,
  Event,
  EventsFilter,
  EventsResult,
} from "./types.js";

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

export class ZNS {
  private url: string;
  private rpcId = 0;

  /**
   * Creates a new ZNS client.
   * @param options.url - Current resolver JSON-RPC endpoint
   */
  constructor(options: { url: string }) {
    this.url = options.url;
  }

  /** Fetch current server status. */
  async status(): Promise<Status> {
    const raw = await this.rpc<Record<string, unknown>>("status");
    return normalizeApiResponse<Status>(raw);
  }

  /** Resolve a ZNS name to its registration. Returns null if not registered.
   *
   *  The name is trimmed and lowercased before querying, so `Alice` and
   *  `alice` resolve the same name.
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
   *  `isAvailable("Alice")` checks "alice". Returns false immediately
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
      toRpcParams((filter ?? {}) as Record<string, unknown>),
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
