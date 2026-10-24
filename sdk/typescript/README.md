# Zcash Name Service TypeScript SDK

TypeScript client for the ZNS resolver. It also makes the request memos that a wallet sends to the ZNS mint.

The SDK does not use private keys. It does not sign or send transactions. The wallet signs and sends each payment.

## Install

```sh
npm install zcashname-sdk
```

## Quick start

```ts
import { ZNS } from "zcashname-sdk";

const zns = new ZNS({ network: "mainnet" });
const name = await zns.resolveName("alice");
if (name) console.log(name.address);

const available = await zns.isAvailable("bob");
console.log(available);
```

The default network is testnet. Set `network` to `"mainnet"` for mainnet names.

## Networks and verification

`ZNS_NETWORKS` contains the data for each network:

- `urls`: the resolver endpoints.
- `viewingKey`: the registry viewing key.
- `treasuryAddress`: the mint treasury address.

```ts
import { ZNS, ZNS_NETWORKS } from "zcashname-sdk";

const zns = new ZNS({ network: "testnet", timeout: 5_000 });
await zns.verify();
```

`verify()` compares the viewing key from the resolver with `ZNS_NETWORKS[network].viewingKey`. If the keys are different, `verify()` throws an error. There are two possible causes:

- The endpoint is not the ZNS resolver.
- The registry key changed after this SDK version. Upgrade the package.

`verify()` does not examine the treasury address.

The client tries the endpoints in the order of `urls`. On mainnet, it tries `main.zcashnames.com` first and `light.zcash.me/zns-mainnet` second. The client tries the next endpoint if an endpoint does not reply, or if it replies with an HTTP 5xx error. The client does not try the next endpoint if a resolver replies with a JSON-RPC error or an HTTP 4xx error.

If you set `url`, the client uses only that endpoint.

## Reverse lookup

```ts
const page = await zns.reverseLookup("u1...", 50, 0);
console.log(page.total, page.items.map((r) => r.name));

const all = await zns.listNames(50, 0);
```

`reverseLookup` gives the names that point to an address. `listNames` gives all registered names. Each result also gives the total number of matches.

## Pricing

```ts
import { annualUsd, foreverUsd } from "zcashname-sdk";

annualUsd("alice"); // 100: USD for each year
foreverUsd("alice"); // 300: USD, one payment
```

The annual price depends on the length of the name:

| Length | USD for each year |
|---|---:|
| 1 character | 10,000 |
| 2 characters | 2,500 |
| 3 characters | 800 |
| 4 characters | 400 |
| 5 characters | 100 |
| 6 or more characters | 20 |

A forever registration costs 3 times the annual price.

The prices are in USD. The mint calculates the ZEC amount at the time of payment. `quoteZats(usd, zatsPerUsd)` converts a USD price to zatoshis for display. The SDK does not contain an exchange rate.

## Mint request memos

Use these functions to make the memo for a claim, an update, a release, or an OTP reply. Send the memo with a payment to the mint treasury address, `ZNS_NETWORKS[network].treasuryAddress`.

```ts
import {
  buildClaimMemo,
  buildUpdateMemo,
  buildReleaseMemo,
} from "zcashname-sdk";

buildClaimMemo({ name: "alice", ua, term: { years: 1 } }); // ZNS:claim:1y:alice:<ua>
buildClaimMemo({ name: "alice", ua, term: "forever", accessCode: "123456" });
buildUpdateMemo({ name: "alice", ua }); // ZNS:update:none:alice:<ua>
buildReleaseMemo({ name: "alice", ua }); // ZNS:release:alice:<ua>
```

A name has 1 to 63 lowercase letters and digits. Remove a `.zcash` or `.zec` suffix before you call a function.

A term is 1 to 99 years, or `"forever"`. An update without a term keeps the current expiry.

An access code has 6 digits. Use it only to claim a protected name.

Each function throws an error if the mint cannot accept the input.

### OTP reply

For an update or a release, the mint sends an OTP memo to the address of the name. Send the same memo back to the treasury address.

```ts
import { parseOtpMemo, buildOtpMemo } from "zcashname-sdk";

const challenge = parseOtpMemo(memoText); // null if the memo is not an OTP memo
if (challenge) {
  const reply = buildOtpMemo(challenge);
}
```

### Address check

The `ua` must be a unified address with an Orchard receiver. The SDK examines the characters and the length of the address. It does not decode the address.

Thus, the SDK cannot find an address that has no Orchard receiver. The mint does not accept a request with such an address.

### Payment URI

```ts
import { buildZip321Uri, ZNS_NETWORKS } from "zcashname-sdk";

const uri = buildZip321Uri({
  address: ZNS_NETWORKS.mainnet.treasuryAddress,
  amount: "0.01",
  memo,
});
```

The SDK does not calculate the payment amount.

## Query data

```ts
const name = await zns.resolveName("alice");
const names = await zns.resolveAddress("u1...");
const allNames = await zns.listAllRegistrations(50, 0);
const history = await zns.events({ name: "alice", limit: 20 });
const status = await zns.status();
```

`resolveName` gives `null` if the name is not registered. The list functions and `events` accept a limit and an offset.

## Configuration

```ts
const zns = new ZNS({ url: "https://your-resolver.example" });
const status = await zns.status();
console.log(status.synced, status.syncedHeight);
```

Use `url` to connect to a different resolver. The client shows the viewing key and the sync state of the resolver. It does not examine the server identity. It does not compare name records with chain data.

## CLI example

The example CLI has four commands: `resolve`, `available`, `status`, and `events`. It has no command that signs. It does not accept private keys.

## License

MIT
