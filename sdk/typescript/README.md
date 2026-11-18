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

## Private lookups

`resolveName` and `isAvailable` never send the name. The client downloads every registered name when it is created, and again every 30 seconds, and looks names up in that list. If the latest download failed, both throw.

Create one client and reuse it.

`resolveNameOnServer` sends the name to the resolver. Use it only where privacy does not matter, such as on a server.

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

annualUsd("alice"); // 188: USD for each year
foreverUsd("alice"); // 752: USD, one payment
```

The annual price depends on the length of the name:

| Length | USD for each year |
|---|---:|
| 1 character | 12,480 |
| 2 characters | 2,720 |
| 3 characters | 920 |
| 4 characters | 472 |
| 5 characters | 188 |
| 6 characters | 112 |
| 7 characters | 48 |
| 8 characters | 32 |
| 9 characters | 24 |
| 10 or more characters | 18 |

A forever registration costs 4 times the annual price.

The prices are in USD. The mint calculates the ZEC amount at the time of payment. `quoteZats(usd, zatsPerUsd)` converts a USD price to zatoshis for display. The SDK does not contain an exchange rate.

## Mint request memos

Use these functions to make the memo for a claim, an update, a release, a marketplace request, or an OTP reply. Send the memo with a payment to the mint treasury address, `ZNS_NETWORKS[network].treasuryAddress`.

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

For an update, a release, a listing or a delisting, the mint sends an OTP memo to the address of the name. Send the same memo back to the treasury address.

```ts
import { parseOtpMemo, buildOtpMemo } from "zcashname-sdk";

const challenge = parseOtpMemo(memoText); // null if the memo is not an OTP memo
if (challenge) {
  const reply = buildOtpMemo(challenge);
}
```

A challenge for a list or an auction also has `terms`: the t1 address the buyer pays, and the price. Check them before you send the reply.

### Marketplace

The owner of a name lists it at a fixed price, or as an auction whose price falls between two block heights. A buyer pays a transparent `t1` address (`tm` on testnet). Amounts are in zatoshis.

```ts
import {
  buildListMemo,
  buildAuctionMemo,
  buildDelistMemo,
  buildBuyMemo,
  parseBuyOtpMemo,
  buyPaymentZats,
} from "zcashname-sdk";

buildListMemo({ name: "alice", ua, payTo: t1, priceZats: 250_000_000 }); // ZNS:list:alice:<ua>:<t1>:2.5
buildAuctionMemo({ name: "alice", ua, payTo: t1, startZats: 500_000_000, minZats: 150_000_000, startBlock: 3_600_000, endBlock: 3_610_000 });
buildDelistMemo({ name: "alice", ua }); // ZNS:delist:alice:<ua>
buildBuyMemo({ name: "alice", ua: buyerUa }); // ZNS:buy:alice:<buyer ua>
```

For a list, an auction or a delist, `ua` is the current address of the name. For a buy, `ua` is the buyer's address: the name moves to it when the sale completes.

The mint answers a buy with a memo to the buyer's address. `parseBuyOtpMemo` reads it. For an auction, the answer also has the price and the time until which the price holds. The buyer pays the price plus the code, read as zatoshis, to the t1 address of the listing:

```ts
const answer = parseBuyOtpMemo(memoText); // null if the memo is not a buy answer
if (answer) {
  const zats = buyPaymentZats(answer.dutch?.priceZats ?? listedPriceZats, answer.code);
}
```

The SDK checks the shape of the t1 address, not its checksum. The mint refuses a listing with a bad t1 address and keeps the fee.

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
