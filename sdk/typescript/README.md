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

const zns = new ZNS({ url: "https://your-resolver.example" });
const name = await zns.resolveName("alice");
if (name) console.log(name.address);

const available = await zns.isAvailable("bob");
console.log(available);
```

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
