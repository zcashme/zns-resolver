# Zcash Name Service TypeScript SDK

Read-only TypeScript client for querying the ZNS resolver.

This package does not accept, load, derive, or sign with private keys. It does not build signed action memos. It currently retains the legacy resolver API while migration to the Name Note resolver API is tracked in [zns-resolver issue #39](https://github.com/zcashme/zns-resolver/issues/39).

## Install

```sh
npm install zcashname-sdk
```

## Quick start

```ts
import { ZNS } from "zcashname-sdk";

const zns = new ZNS();
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
const listings = await zns.listings(50, 0);
const history = await zns.events({ name: "alice", limit: 20 });
const status = await zns.status();
```

`resolveName` returns `null` when the name is not registered. List and event methods support pagination.

## Configuration

```ts
const zns = new ZNS({ network: "testnet" });
const custom = new ZNS({ url: "https://your-indexer.example/zns" });
await custom.verify(); // checks that the reported UIVK matches the selected network
```

The optional `verify()` check identifies a configured resolver by its UIVK. It does not verify a name binding against chain data.

## CLI example

The example CLI supports read-only queries such as `resolve`, `available`, `listings`, `status`, `events`, and `cost`. It has no signing commands and accepts no private-key input.

## License

MIT
