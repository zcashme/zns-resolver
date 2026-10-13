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

The client ships per-network endpoint presets and the registry viewing keys
the live resolvers serve today:

```ts
import { ZNS, ZNS_NETWORKS } from "zcashname-sdk";

const zns = new ZNS({ network: "testnet", timeout: 5_000 });
await zns.verify(); // throws unless status().viewingKey matches the baked key
```

`verify()` fails closed: either the endpoint is not the ZNS resolver, or this
SDK predates a registry rotation — upgrade the package.

> ⚠️ **0.14.0-rc releases carry placeholder registry keys** (mainnet still
> runs the legacy indexer, whose key is UIVK-era). The final ceremony keys
> will replace them and the rc line will be superseded. Do not pin `rc.*`
> in anything durable.

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
