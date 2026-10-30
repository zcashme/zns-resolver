# Changelog

## 0.8.0

- BREAKING: ported to the ZNS resolver API for the mainnet launch, matching
  the TypeScript SDK 0.17.0. The package now lives in
  `zcashme/zns-resolver`.
- New: mint request memos (`buildClaimMemo`, `buildUpdateMemo`,
  `buildReleaseMemo`, `buildOtpMemo`, `parseOtpMemo`), `buildZip321Uri`,
  and USD pricing (`annualUsd`, `foreverUsd`, `quoteZats`).
- New: `reverseLookup` and `listNames` return a `Paginated` page with the
  total match count. `resolveAddress` and `listAllRegistrations` use them
  too, so an empty or name-shaped address gives an empty list.
- `EventsFilter.name` is normalized like `resolveName`.
- New: on mainnet the client falls back to the next endpoint when one does
  not reply or replies with an HTTP 5xx error.
- `verify()` compares the registry viewing key and throws
  `ZnsVerificationException` on a mismatch. `networks` is now
  `znsNetworks`, with `urls`, `viewingKey` and `treasuryAddress`.
- `Registration` and `Event` match the resolver records: `expiresAt` is new;
  `nonce`, `signature` and `listing` are gone. `ZnsAction` is `claim`,
  `update` or `release`.
- Names are 1-63 characters. `normalizeName` trims and lowercases ASCII
  only; it no longer strips a `.zcash` or `.zec` suffix.
- `isValidUnifiedAddress` checks the length and the bech32m checksum.
- Removed: listings, buy, `prepare*` actions, Merkle proofs, signature
  verification, `validatePayload`, `isValidTransparentAddress`,
  `buildZcashUri` and the claim cost helpers. The `cryptography`, `bech32`
  and `meta` dependencies are gone.

## 0.7.0

- BREAKING: sovereign signatures removed, matching the indexer (all actions
  are now strictly admin-signed). `Registration`, `Listing`, and `Event` no
  longer carry a `pubkey` field; `complete()` no longer accepts a
  `userPubkey` and never emits the `:<pubkey>` memo suffix;
  `verifyRegistration`/`verifyListing` verify strictly against the admin
  pubkey; `verifySovereignSignature` is renamed to `verifySignature`;
  `hashLeaf` matches the indexer's new Merkle leaf pre-image (no pubkey
  bytes).

## 0.6.0

- Feat: Merkle inclusion proofs. `resolveNameWithProof` returns a
  `RegistrationWithProof` carrying a `MerkleProof` against the indexer's
  committed state root, and `verifyProof` checks the binding offline by
  recomputing the BLAKE2b leaf/path (`hashLeaf` and `hashInternal` are also
  exported).
- Feat: lenient name input. All name-taking methods (`resolveName`,
  `resolveNameWithProof`, `isAvailable`, and every `prepare*` action) now
  normalize their input: trim, lowercase, and strip one trailing
  `.zcash`/`.zec` suffix (case-insensitive), so `Alice.zcash`, `aLice.Zec`,
  and `alice` are equivalent. The normalizer is exported as `normalizeName`.
- Names that are invalid after normalization resolve to `null` (or `false`
  from `isAvailable`) locally, without a server round trip; `prepare*`
  actions still throw `InvalidNameException`.

## 0.5.2

- Fix: `resolveName` now normalizes the input to lowercase before querying the
  indexer, so `resolveName('Alice')` and `resolveName('alice')` are equivalent.
- Fix: all RPC calls now enforce a configurable timeout (default 10 s) via
  `RpcClient`, preventing indefinite hangs when the indexer is slow or
  unreachable. The `ZNS` factory accepts an optional `timeout` parameter to
  override the default.

## 0.5.1

- Fix: expose correct UIVK strings for testnet and mainnet networks.

## 0.5.0

- Initial public release.
- `ZNS` client with full read surface: `resolveName`, `resolveAddress`, `listAllRegistrations`, `isAvailable`, `listings`, `events`.
- Prepared actions (`prepareClaim`, `prepareList`, `prepareBuy`, `prepareUpdate`, `prepareDelist`, `prepareRelease`, `prepareSetPrice`) with sealed `PreparedAction` hierarchy for exhaustive switching.
- Ed25519 signature verification via `verifyEd25519`, `verifyRegistration`, `verifyListing`, `verifySovereignSignature`.
- ZIP-321 URI builder and parser (`buildZcashUri`, `parseZip321Uri`).
- Payload validation mirroring the indexer's `parse_memo` logic.
- Typed exceptions: `InvalidNameException`, `InvalidAddressException`, `ZnsRpcException`.
- Testnet and mainnet network configs with UIVK pinning via `verify()`.
