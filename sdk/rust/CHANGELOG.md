# Changelog

## 0.2.0 (unreleased)

- Bring the Rust SDK to feature parity with Dart SDK 0.8.0: claim, update,
  release and OTP memo builders; OTP parsing; ZIP-321 payment URI helpers;
  unified address validation; and annual/forever USD pricing with zatoshi
  display quotes.
- Add testnet/mainnet presets, registry viewing key verification, a testnet
  default, configurable per-endpoint timeouts, shared HTTP clients and ordered
  endpoint fallback. Explicit endpoint overrides replace the preset list.
- Add `resolve_address` and `list_all_registrations` page-item shorthands.
- Normalize event name filters like name lookups.
- Match TypeScript payment URI parsing: keep the first duplicate amount or
  memo and preserve payment fields when another parameter has malformed encoding.
- Breaking: remove the nonexistent `Status.registered` field, change `synced`
  to `Option<bool>`, default a missing `synced_height` to zero, and add optional
  `treasury_address`, matching current resolver responses and Dart behavior.
- Add local HTTP integration tests and Dart/mint-compatible helper vectors.

## 0.1.0

- Initial typed read client for resolver JSON-RPC, with canonical name
  validation, pagination, events and response ID checking.
