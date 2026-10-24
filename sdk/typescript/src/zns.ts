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

/**
 * Lengths of a mainnet UA the mint accepts: an Orchard receiver, optionally
 * Sapling and/or transparent, nothing else. Testnet adds 4 (`utest` vs `u`).
 */
const MAINNET_UA_LENGTHS = [106, 141, 178, 213];
const TESTNET_UA_LENGTHS = MAINNET_UA_LENGTHS.map((n) => n + 4);

/**
 * Validates a Zcash unified address by length and bech32m checksum. It does
 * not decode the receivers, so it cannot see whether an Orchard receiver is
 * present: a Sapling-only UA has the same length as an Orchard-only one.
 */
function isValidUnifiedAddress(address: string): boolean {
  const lengths = address.startsWith("utest1") ? TESTNET_UA_LENGTHS : MAINNET_UA_LENGTHS;
  if (!lengths.includes(address.length) || address !== address.toLowerCase()) return false;
  try {
    // A UA is longer than bech32's default 90-character limit.
    const { prefix } = bech32m.decode(address, 1000);
    return prefix === "u" || prefix === "utest";
  } catch {
    return false;
  }
}

// ── Mint request memos ───────────────────────────────────────────────────────
//
//   Claim:                   ZNS:claim:<term>:<name>:<ua>
//   Claim a protected name:  ZNS:claim:<access code>:<term>:<name>:<ua>
//   Update:                  ZNS:update:<term or none>:<name>:<ua>
//   Release:                 ZNS:release:<name>:<ua>
//   OTP reply:               ZNS:otp:<otp code>:<name>:<update or release>:<ua>

/** A registration term: 1–99 whole years, or forever. */
export type ZnsTerm = { years: number } | "forever";

/** Actions the mint challenges with an OTP. Claims are never challenged. */
export type OtpAction = "update" | "release";

export interface OtpMemo {
  code: string;
  name: string;
  action: OtpAction;
  ua: string;
}

const MAX_TERM_YEARS = 99;
const SIX_DIGITS_RE = /^\d{6}$/;

/** ZIP-321 limits: the largest payment amount in ZEC, and the memo size. */
const MAX_MONEY = 21_000_000;
const MAX_MEMO_BYTES = 512;

function termField(term: ZnsTerm): string {
  if (term === "forever") return "forever";
  const { years } = term;
  if (!Number.isInteger(years) || years < 1 || years > MAX_TERM_YEARS) {
    throw new Error("ZNS: a term is 1-99 years, or forever.");
  }
  return `${years}y`;
}

/** Claim request: `ZNS:claim:<term>:<name>:<ua>`. To claim a protected name,
 *  set `accessCode`: `ZNS:claim:<access code>:<term>:<name>:<ua>`. */
export function buildClaimMemo(input: {
  name: string;
  ua: string;
  term: ZnsTerm;
  accessCode?: string;
}): string {
  const { name, ua, accessCode } = input;
  if (accessCode !== undefined && !SIX_DIGITS_RE.test(accessCode)) {
    throw new Error("ZNS: the access code is six digits.");
  }
  const term = termField(input.term);
  if (!isValidName(name)) {
    throw new Error("ZNS: a name is 1-63 characters: lowercase letters and digits only.");
  }
  if (!isValidUnifiedAddress(ua)) {
    throw new Error("ZNS: not a unified address the mint accepts.");
  }
  return accessCode === undefined
    ? `ZNS:claim:${term}:${name}:${ua}`
    : `ZNS:claim:${accessCode}:${term}:${name}:${ua}`;
}

/** Update request: `ZNS:update:<term or none>:<name>:<ua>`. Without a
 *  `term` the current expiry carries forward. */
export function buildUpdateMemo(input: {
  name: string;
  ua: string;
  term?: ZnsTerm;
}): string {
  const { name, ua } = input;
  const term = input.term === undefined ? "none" : termField(input.term);
  if (!isValidName(name)) {
    throw new Error("ZNS: a name is 1-63 characters: lowercase letters and digits only.");
  }
  if (!isValidUnifiedAddress(ua)) {
    throw new Error("ZNS: not a unified address the mint accepts.");
  }
  return `ZNS:update:${term}:${name}:${ua}`;
}

/** Release request: `ZNS:release:<name>:<ua>`. */
export function buildReleaseMemo(input: { name: string; ua: string }): string {
  const { name, ua } = input;
  if (!isValidName(name)) {
    throw new Error("ZNS: a name is 1-63 characters: lowercase letters and digits only.");
  }
  if (!isValidUnifiedAddress(ua)) {
    throw new Error("ZNS: not a unified address the mint accepts.");
  }
  return `ZNS:release:${name}:${ua}`;
}

/** OTP memo: `ZNS:otp:<otp code>:<name>:<update or release>:<ua>`. The mint sends
 *  it as the challenge; the wallet sends the same text back. */
export function buildOtpMemo(input: OtpMemo): string {
  const { code, name, action, ua } = input;
  if (action !== "update" && action !== "release") {
    throw new Error("ZNS: only updates and releases are challenged.");
  }
  if (!SIX_DIGITS_RE.test(code)) {
    throw new Error("ZNS: the OTP code is six digits.");
  }
  if (!isValidName(name)) {
    throw new Error("ZNS: a name is 1-63 characters: lowercase letters and digits only.");
  }
  if (!isValidUnifiedAddress(ua)) {
    throw new Error("ZNS: not a unified address the mint accepts.");
  }
  return `ZNS:otp:${code}:${name}:${action}:${ua}`;
}

/** Read a mint challenge. Returns null for anything that is not an OTP memo.
 *  Trailing NUL padding from a 512-byte memo field is ignored. */
export function parseOtpMemo(text: string): OtpMemo | null {
  const parts = text.replace(/\0+$/, "").split(":");
  if (parts.length !== 6 || parts[0] !== "ZNS" || parts[1] !== "otp") return null;
  const [, , code, name, action, ua] = parts;
  if (!SIX_DIGITS_RE.test(code) || !isValidName(name) || !isValidUnifiedAddress(ua)) return null;
  if (action !== "update" && action !== "release") return null;
  return { code, name, action, ua };
}

function toBase64Url(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** ZIP-321 payment URI: `zcash:<address>[?amount=<ZEC>][&memo=<base64url>]`.
 *  `amount` is a decimal ZEC string with at most 8 decimal places. */
export function buildZip321Uri(input: {
  address: string;
  amount?: string;
  memo?: string;
}): string {
  if (!input.address || /[?&:\s]/.test(input.address)) {
    throw new Error("ZNS: a payment URI needs an address.");
  }
  const params: string[] = [];
  if (input.amount !== undefined) {
    if (!/^\d+(\.\d{1,8})?$/.test(input.amount)) {
      throw new Error("ZNS: the amount is a decimal ZEC value with at most 8 decimal places.");
    }
    // Compare the decimal string itself, not a floating-point conversion.
    const [whole, fraction = ""] = input.amount.split(".");
    if (Number(whole) > MAX_MONEY || (Number(whole) === MAX_MONEY && /[1-9]/.test(fraction))) {
      throw new Error(`ZNS: the amount is more than ${MAX_MONEY} ZEC.`);
    }
    params.push(`amount=${input.amount}`);
  }
  if (input.memo) {
    if (input.address.startsWith("t")) {
      throw new Error("ZNS: a transparent address cannot receive a memo.");
    }
    if (new TextEncoder().encode(input.memo).length > MAX_MEMO_BYTES) {
      throw new Error(`ZNS: the memo is more than ${MAX_MEMO_BYTES} bytes.`);
    }
    params.push(`memo=${toBase64Url(input.memo)}`);
  }
  return params.length
    ? `zcash:${input.address}?${params.join("&")}`
    : `zcash:${input.address}`;
}

/** The ZNS networks. */
export type Network = "mainnet" | "testnet";

/**
 * Registry keys, treasury addresses and endpoints, as the live resolvers
 * report them in `status`. `urls` is tried in order: the first is primary,
 * the rest are fallbacks used only when an endpoint cannot be reached.
 * `verify()` compares the server's viewing key against
 * `viewingKey`: a mismatch means the endpoint is not this registry, or this
 * SDK predates a key rotation.
 */
export const ZNS_NETWORKS = {
  testnet: {
    urls: ["https://light.zcash.me/zns-testnet"],
    viewingKey:
      "uviewtest1gkq78nsemsvs4qekadrv6qlqdmvcv2cct8k3c8cwycedpk90p8rtkx24qe8q73gg2ark8crqmmaavrerf7cxu0sp50an54crmeaalfxl72aetxnr89ksad4lvxzmnkyejhhwn65euzamt9wsukgfemeayhcesreasarmq600lfeju3qenh05e5e5cz6nhhsw3cfdcd3d08d0nxswxmkkxl2fq0hzv6t5dm3ftnz4y76236a49v3us4uq2tlsewf86tjtfwm6kue348me04rm7dw6uawv8myw7t5tyx2t453clhjz5f55lvh7hnvr8g8zt8lf4sy5rzq23ntp7g5ly6kyz2nkwfek06llhjtuz2fft9rg6cuuwg4n7299vajnmq6swg3sf563r0jjzuwuvfpjhmfmydar9dcu3fkuawt6u8qu08vhg5mthrcggrjuhht6f0perh0tqmdfrtgzvhjymay7h9ymvfxgd4r56uz6zrsf5gnvw5jy",
    treasuryAddress:
      "utest1tquw9k9qlsphhf30tau2qh758vfvf7xkvjea3rlc5mj4cjscgzfzgjeafjnl7r8tvv0akq3v2ze5q0azf4lnd4vf7kusrfensdsrr7tsgruvuqk0dv0hepnrk0pzgz3fj0mxc0lr29yf65nkwvw74gzep4792rkqa769lxl0sua09s0r",
  },
  mainnet: {
    urls: ["https://main.zcashnames.com", "https://light.zcash.me/zns-mainnet"],
    viewingKey:
      "uview1209ld9y4fr2sel9q87kfjg7xm2sy53e3qf7dvcw3w8jwhhjpldl8z0vk0a6hveg4jk7xcn0rc9mr8yg77djfpc0uxrm5pqzt8q7ul4w0nvs3x5q9vmk9nm2crywtqaealek4mp47ur7ywu7g4g4xcdlwv6gnqzvu5panv5je2ps6rxmx4856unlt5wde8fpz8uhrqchmwhn2j252h3cyx5phad877xezerm6ex8l5kcfjw02ll7m0nspfufamh3ae9ea48395elptspnv6vvc70j2gcjrdze9syt8kpr2kc67flvg8687thdkpha7lpt9r4p9wjty90q2v2yjanqsr2zrecrqs7lfx9cpkmshf7jyh2vvwfqnyzwtpuy7p72hm2mjhperdafvm9u5rwa6s6urcy0uw30f687e3jnmas45qfqtwyz39kf6kxklfrdn7t4hkz56jegmlr8s8hwmt9wa8vz8zlmm76lrmzyj82nzpeqfu572njq",
    treasuryAddress:
      "u1f68wxdxsrswjl04kcc397a9ljedg88rl8dgazrk8l9v8jx6t3gqsktvqyvmc3ypgf4pum03q4ftvy5s0fe803nrxm8kw6dgzlvnv83nv",
  },
} as const satisfies Record<
  Network,
  { urls: readonly string[]; viewingKey: string; treasuryAddress: string }
>;

/** A resolver answered, so its reply is final and is not retried elsewhere. */
class ZnsResponseError extends Error {}

export class ZNS {
  /** The primary resolver JSON-RPC endpoint. */
  url: string;
  /** The network preset in use. */
  network: Network;
  private readonly urls: readonly string[];
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
    this.urls =
      options.url != null ? [options.url] : ZNS_NETWORKS[this.network].urls;
    this.url = this.urls[0];
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
   *  Checks the length and the bech32m checksum, but does NOT decode the
   *  receivers.
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

    // Endpoints are tried in preset order. Only a failure to get an answer
    // moves on to the next one; a resolver's own reply, even an error, is final.
    let lastError: unknown;
    for (const url of this.urls) {
      try {
        return await this.post<T>(url, body, signal);
      } catch (error) {
        if (signal?.aborted || error instanceof ZnsResponseError) throw error;
        lastError = error;
      }
    }
    throw lastError;
  }

  private async post<T>(
    url: string,
    body: string,
    signal?: AbortSignal,
  ): Promise<T> {
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
      const res = await fetch(url, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body,
        signal: controller.signal,
      });

      if (!res.ok) {
        const message = `ZNS HTTP ${res.status}: ${res.statusText}`;
        // Server errors may be this node's alone; client errors are the answer.
        throw res.status >= 500
          ? new Error(message)
          : new ZnsResponseError(message);
      }

      const json = (await res.json()) as {
        result?: T;
        error?: { code: number; message: string };
      };

      if (json.error) {
        throw new ZnsResponseError(
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
