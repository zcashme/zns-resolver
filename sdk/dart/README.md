# Zcash Name Service Dart SDK

Dart client for the ZNS resolver. It also makes the request memos that a wallet sends to the ZNS mint.

The SDK does not use private keys. It does not sign or send transactions. The wallet signs and sends each payment.

## Install

```sh
dart pub add zcashname_sdk
```

## Quick start

```dart
import 'package:zcashname_sdk/zcashname_sdk.dart';

final zns = ZNS(network: Network.mainnet);
final name = await zns.resolveName('alice');
if (name != null) print(name.address);

final available = await zns.isAvailable('bob');
print(available);
```

The default network is testnet. Set `network` to `Network.mainnet` for mainnet names.

## Networks and verification

`znsNetworks` contains the data for each network:

- `urls`: the resolver endpoints.
- `viewingKey`: the registry viewing key.
- `treasuryAddress`: the mint treasury address.

```dart
final zns = ZNS(network: Network.testnet, timeout: const Duration(seconds: 5));
await zns.verify();
```

`verify()` compares the viewing key from the resolver with `znsNetworks[network]!.viewingKey`. If the keys are different, `verify()` throws a `ZnsVerificationException`. There are two possible causes:

- The endpoint is not the ZNS resolver.
- The registry key changed after this SDK version. Upgrade the package.

`verify()` does not examine the treasury address.

The client tries the endpoints in the order of `urls`. On mainnet, it tries `main.zcashnames.com` first and `light.zcash.me/zns-mainnet` second. The client tries the next endpoint if an endpoint does not reply in time, or if it replies with an HTTP 5xx error. The client does not try the next endpoint if a resolver replies with a JSON-RPC error or an HTTP 4xx error. In both of those cases it throws a `ZnsRpcException`.

If you set `url`, the client uses only that endpoint.

`timeout` applies to each request to one endpoint. The default is 10 seconds.

## Reverse lookup

```dart
final page = await zns.reverseLookup('u1...', limit: 50, offset: 0);
print('${page.total} ${page.items.map((r) => r.name)}');

final all = await zns.listNames(limit: 50, offset: 0);
```

`reverseLookup` gives the names that point to an address. `listNames` gives all registered names. Each result also gives the total number of matches.

## Pricing

```dart
annualUsd('alice'); // 100: USD for each year
foreverUsd('alice'); // 300: USD, one payment
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

Use these functions to make the memo for a claim, an update, a release, or an OTP reply. Send the memo with a payment to the mint treasury address, `znsNetworks[network]!.treasuryAddress`.

```dart
buildClaimMemo(name: 'alice', ua: ua, term: const ZnsTerm.years(1)); // ZNS:claim:1y:alice:<ua>
buildClaimMemo(name: 'alice', ua: ua, term: ZnsTerm.forever, accessCode: '123456');
buildUpdateMemo(name: 'alice', ua: ua); // ZNS:update:none:alice:<ua>
buildReleaseMemo(name: 'alice', ua: ua); // ZNS:release:alice:<ua>
```

A name has 1 to 63 lowercase letters and digits. Remove a `.zcash` or `.zec` suffix before you call a function.

A term is 1 to 99 years, or `ZnsTerm.forever`. An update without a term keeps the current expiry.

An access code has 6 digits. Use it only to claim a protected name.

Each function throws a `FormatException` if the mint cannot accept the input.

### OTP reply

For an update or a release, the mint sends an OTP memo to the address of the name. Send the same memo back to the treasury address.

```dart
final challenge = parseOtpMemo(memoText); // null if the memo is not an OTP memo
if (challenge != null) {
  final reply = buildOtpMemo(challenge);
}
```

### Address check

The `ua` must be a unified address with an Orchard receiver. The SDK examines the length and the bech32m checksum of the address. It does not decode the address.

Thus, the SDK cannot find an address that has no Orchard receiver. The mint does not accept a request with such an address.

### Payment URI

```dart
final uri = buildZip321Uri(
  address: znsNetworks[Network.mainnet]!.treasuryAddress,
  amount: '0.01',
  memo: memo,
);
```

The SDK does not calculate the payment amount.

## Query data

```dart
final name = await zns.resolveName('alice');
final names = await zns.resolveAddress('u1...');
final allNames = await zns.listAllRegistrations(limit: 50, offset: 0);
final history = await zns.events(const EventsFilter(name: 'alice', limit: 20));
final status = await zns.status();
```

`resolveName` gives `null` if the name is not registered. The list functions and `events` accept a limit and an offset.

## Configuration

```dart
final zns = ZNS(url: Uri.parse('https://your-resolver.example'));
final status = await zns.status();
print('${status.synced} ${status.syncedHeight}');
```

Use `url` to connect to a different resolver. The client shows the viewing key and the sync state of the resolver. It does not examine the server identity. It does not compare name records with chain data.

Pass `httpClient` to use your own `http.Client`. Otherwise call `close()` when you are done with the client.

## License

MIT
