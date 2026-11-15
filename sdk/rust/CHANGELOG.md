# Changelog

## 0.2.0 (unreleased)

- `Action` accepts `sale`, `list`, `auction`, and `delist`.
- Pricing matches the mint: nine tiers for 1–9 character names (12,480 / 2,720
  / 920 / 472 / 188 / 112 / 48 / 32 / 24 USD), a minimum of 18 USD from 10
  characters, and forever at four annual prices.
- Marketplace memos: `build_list_memo`, `build_auction_memo`,
  `build_delist_memo`, `build_buy_memo`, `parse_buy_otp_memo` and
  `buy_payment_zats`. `OtpAction` adds `List`, `Auction` and `Delist`, and
  `OtpMemo` carries a list or auction challenge's `terms`.
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
- `build_zip321_uri` accepts only ASCII letters and digits in the address, so a
  `#` or other URI delimiter cannot move the amount or memo out of the query.
- `parse_zip321_uri` reads the ZIP 321 `address` query field when the URI has
  no address before `?`.
- Breaking: remove the nonexistent `Status.registered` field, change `synced`
  to `Option<bool>`, default a missing `synced_height` to zero, and add optional
  `treasury_address`, matching current resolver responses and Dart behavior.
- Add local HTTP integration tests and Dart/mint-compatible helper vectors.

## 0.1.0

- Initial typed read client for resolver JSON-RPC, with canonical name
  validation, pagination, events and response ID checking.
