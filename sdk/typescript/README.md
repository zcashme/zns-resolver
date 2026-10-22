# Zcash Name Service TypeScript SDK

Read-only TypeScript client for querying the ZNS resolver.

This package is a read-only client for the current Name Note resolver JSON-RPC API. It does not accept, load, derive, or sign with private keys, and it does not build Name Note transactions.

## Install

```sh
npm install zcashname-sdk
```

## Quick start

```ts
import { ZNS } from "zcashname-sdk";

// Pick a network preset (testnet is the default), or pass an explicit url
// which always wins:
const zns = new ZNS({ network: "mainnet" });
const name = await zns.resolveName("alice");
if (name) console.log(name.address);

const available = await zns.isAvailable("bob");
console.log(available);
```

## Networks and verification

The client ships per-network endpoint presets, the registry viewing keys, and
the mint's treasury addresses, as the live resolvers serve them today:

```ts
import { ZNS, ZNS_NETWORKS } from "zcashname-sdk";

const zns = new ZNS({ network: "testnet", timeout: 5_000 });
await zns.verify(); // throws unless status().viewingKey matches the baked key
```

`verify()` fails closed: either the endpoint is not the ZNS resolver, or this
SDK predates a registry rotation — upgrade the package.

`ZNS_NETWORKS[network].treasuryAddress` is the same address `status()` returns
as `treasuryAddress`. `verify()` checks the viewing key only.

Each preset lists its endpoints in `ZNS_NETWORKS[network].urls`. Mainnet tries
`main.zcashnames.com` first and falls back to `light.zcash.me/zns-mainnet` when
it cannot be reached. A resolver's own error reply is returned as is, with no
fallback. Passing `url` to the constructor uses that one endpoint only.

## Reverse lookup

```ts
// All names bound to an address, with the total match count:
const page = await zns.reverseLookup("u1...", 50, 0);
console.log(page.total, page.items.map(r => r.name));

// All registered names, with the total:
const all = await zns.listNames(50, 0);
```

These use the resolver's typed `reverse_lookup` / `list_names` verbs and
require the current (Name Note) resolver. The legacy mainnet indexer does
not implement them yet.

## Pricing

The SDK carries the pricing law (mirrors the mint):

```ts
import { ZNS_PRICING, annualUsd, foreverUsd } from "zcashname-sdk";

annualUsd("alice"); // 100 — USD per year (5-char tier)
foreverUsd("alice"); // 300 — one-time
```

Tiers: 1-char $10,000/yr · 2-char $2,500 · 3-char $800 · 4-char $400 ·
5-char $100 · 6+ chars $20. Forever costs 3× annual.

Prices are **USD**. The ZEC amount charged at payment is computed by the
mint. `quoteZats(usd, zatsPerUsd)` converts for display when a rate is
available; no exchange rate is baked into the SDK.

## Query data

```ts
const name = await zns.resolveName("alice");
const names = await zns.resolveAddress("u1...");
const allNames = await zns.listAllRegistrations(50, 0);
const history = await zns.events({ name: "alice", limit: 20 });
const status = await zns.status();
```

`resolveName` returns `null` when the name is not registered. List and event methods support pagination.

## Configuration

```ts
const zns = new ZNS({ url: "https://your-resolver.example" });
const status = await zns.status();
console.log(status.synced, status.syncedHeight);
```

Pass the current resolver's JSON-RPC URL explicitly. The client reports the resolver's viewing key and sync state; it does not independently authenticate the server or verify name bindings against chain data.

## CLI example

The example CLI supports read-only queries: `resolve`, `available`, `status`, and `events`. It has no signing commands and accepts no private-key input.

## License

MIT
