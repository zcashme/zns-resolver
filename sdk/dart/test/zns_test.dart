import 'dart:async';
import 'dart:convert';

import 'package:http/http.dart' as http;
import 'package:http/testing.dart';
import 'package:test/test.dart';
import 'package:zcashname_sdk/zcashname_sdk.dart';

const _ua =
    'utest100qlkeru5c3m5kfrwe2hsmcfzmusreaza2prdyelg2kd2tr2842nceq952vay3gpmgky09fgft4z57h4z2zqzz5rcwgd4q90u54ek5yyca4s6e6y2jja9sww27kzedzznjcupcu0svq2exvq995c0lhl5zm53g4ksnm2xuwt3snv4dgh';
final _resolver = Uri.parse('https://resolver.example');

final _record = {
  'name': 'alice',
  'address': _ua,
  'txid': 'ab' * 32,
  'height': 100,
  'last_action': 'claim',
  'expires_at': 'none',
};

/// A resolver that answers every request with [result] and records each
/// request body.
(ZNS, List<Map<String, dynamic>>) _client(Object? result,
    {Network network = Network.testnet}) {
  final requests = <Map<String, dynamic>>[];
  final client = MockClient((request) async {
    requests.add(jsonDecode(request.body) as Map<String, dynamic>);
    return http.Response(jsonEncode({'jsonrpc': '2.0', 'result': result}), 200);
  });
  return (
    ZNS(network: network, url: _resolver, httpClient: client),
    requests,
  );
}

void main() {
  group('network presets', () {
    test('defaults to the testnet preset (fail safe)', () {
      final z = ZNS();
      expect(z.network, Network.testnet);
      expect(z.url, Uri.parse('https://light.zcash.me/zns-testnet'));
    });

    test('selects the mainnet preset by network', () {
      final z = ZNS(network: Network.mainnet);
      expect(z.network, Network.mainnet);
      expect(z.url, Uri.parse('https://main.zcashnames.com'));
    });

    test('an explicit url always wins over the preset', () {
      final z = ZNS(network: Network.mainnet, url: _resolver);
      expect(z.network, Network.mainnet);
      expect(z.url, _resolver);
    });
  });

  group('resolveName normalization', () {
    test('sends the normalized name as the query', () async {
      final (zns, requests) = _client(_record);
      for (final input in ['Alice', 'alice', 'aLiCe', '  ALICE  ']) {
        final reg = await zns.resolveName(input);
        expect(reg?.name, 'alice');
        expect(reg?.lastAction, ZnsAction.claim);
        expect(reg?.expiresAt, 'none');
      }
      expect(requests.map((r) => r['params']['query']),
          ['alice', 'alice', 'alice', 'alice']);
    });

    test('returns null for an unregistered name', () async {
      final (zns, _) = _client(null);
      expect(await zns.resolveName('bob'), isNull);
      expect(await zns.isAvailable('bob'), isTrue);
    });

    test('returns null for invalid names without hitting the server', () async {
      final (zns, requests) = _client(_record);
      expect(await zns.resolveName('alice.eth'), isNull);
      expect(await zns.resolveName('alice.zec'), isNull);
      expect(await zns.resolveName(''), isNull);
      expect(await zns.isAvailable('alice.eth'), isFalse);
      expect(requests, isEmpty);
    });

    test('isAvailable checks the normalized name', () async {
      final (zns, requests) = _client(_record);
      expect(await zns.isAvailable('Alice'), isFalse);
      expect(requests.single['params']['query'], 'alice');
    });

    test('throws on an action the resolver does not send', () async {
      final (zns, _) = _client({..._record, 'last_action': 'buy'});
      expect(zns.resolveName('alice'), throwsFormatException);
    });
  });

  group('resolve list forms', () {
    test('resolveAddress queries the address with pagination', () async {
      final (zns, requests) = _client([_record]);
      final regs = await zns.resolveAddress(_ua, limit: 10, offset: 5);
      expect(regs.single.name, 'alice');
      expect(
          requests.single['params'], {'query': _ua, 'limit': 10, 'offset': 5});
    });

    test('listAllRegistrations sends the empty query', () async {
      final (zns, requests) = _client(<Object>[]);
      expect(await zns.listAllRegistrations(), isEmpty);
      expect(requests.single['params'], {'query': ''});
    });
  });

  group('reverseLookup and listNames', () {
    test('calls the typed reverse_lookup verb and returns the total', () async {
      final (zns, requests) = _client({
        'items': [_record],
        'total': 3,
        'limit': 1,
        'offset': 0,
      });
      final page = await zns.reverseLookup(_ua, limit: 1, offset: 0);
      expect(requests.single['method'], 'reverse_lookup');
      expect(
          requests.single['params'], {'address': _ua, 'limit': 1, 'offset': 0});
      expect(page.total, 3);
      expect(page.items.single.name, 'alice');
      expect(page.items.single.lastAction, ZnsAction.claim);
    });

    test('calls the typed list_names verb', () async {
      final (zns, requests) =
          _client({'items': <Object>[], 'total': 0, 'limit': 50, 'offset': 0});
      final page = await zns.listNames(limit: 50, offset: 10);
      expect(requests.single['method'], 'list_names');
      expect(requests.single['params'], {'limit': 50, 'offset': 10});
      expect(page.items, isEmpty);
    });
  });

  group('events', () {
    test('sends lowercase event filters using resolver parameter names',
        () async {
      final (zns, requests) = _client({
        'events': [
          {
            'id': 1,
            'name': 'alice',
            'action': 'update',
            'txid': 'ab' * 32,
            'height': 123,
            'action_index': 2,
            'address': _ua,
            'expires_at': 'none',
          }
        ],
        'total': 1,
        'limit': 50,
        'offset': 0,
      });
      final result = await zns.events(const EventsFilter(
          name: 'alice', action: ZnsAction.update, sinceHeight: 100));
      expect(requests.single['params'],
          {'name': 'alice', 'action': 'update', 'since_height': 100});
      final event = result.events.single;
      expect(event.action, ZnsAction.update);
      expect(event.actionIndex, 2);
      expect(event.address, _ua);
      expect(event.expiresAt, 'none');
      expect((result.total, result.limit, result.offset), (1, 50, 0));
    });

    test('sends empty params without a filter', () async {
      final (zns, requests) =
          _client({'events': <Object>[], 'total': 0, 'limit': 50, 'offset': 0});
      await zns.events();
      expect(requests.single['params'], isEmpty);
    });
  });

  group('status and verify', () {
    test('passes when the server serves the baked viewing key', () async {
      for (final network in Network.values) {
        final (zns, requests) = _client({
          'synced_height': 1,
          'viewing_key': znsNetworks[network]!.viewingKey,
        }, network: network);
        await zns.verify();
        expect(requests.single['method'], 'status');
      }
    });

    test('reads every status field', () async {
      final testnet = znsNetworks[Network.testnet]!;
      final (zns, _) = _client({
        'synced_height': 4529119,
        'synced': true,
        'viewing_key': testnet.viewingKey,
        'treasury_address': testnet.treasuryAddress,
      });
      final status = await zns.status();
      expect(status.syncedHeight, 4529119);
      expect(status.synced, isTrue);
      expect(status.viewingKey, testnet.viewingKey);
      expect(status.treasuryAddress, testnet.treasuryAddress);
    });

    test('throws with upgrade guidance on a key mismatch', () async {
      final (zns, _) =
          _client({'synced_height': 1, 'viewing_key': 'uviewtest1rotated'});
      expect(
        zns.verify(),
        throwsA(isA<ZnsVerificationException>().having((e) => e.message,
            'message', contains('registry rotation. Upgrade zcashname_sdk'))),
      );
    });
  });

  group('timeout', () {
    test('fails a request that outlasts the timeout', () async {
      final client = MockClient((_) => Completer<http.Response>().future);
      final zns = ZNS(
          url: _resolver,
          httpClient: client,
          timeout: const Duration(milliseconds: 20));
      expect(zns.status(), throwsA(isA<TimeoutException>()));
    });
  });

  group('endpoint fallback', () {
    const okStatus = {
      'result': {'synced_height': 1, 'viewing_key': 'uview-x'}
    };
    final primary = Uri.parse('https://main.zcashnames.com');

    /// A mainnet client whose endpoints answer with [respond], recording the
    /// URL of each request.
    (ZNS, List<Uri>) mainnet(Future<http.Response> Function(Uri) respond) {
      final urls = <Uri>[];
      final client = MockClient((request) {
        urls.add(request.url);
        return respond(request.url);
      });
      return (ZNS(network: Network.mainnet, httpClient: client), urls);
    }

    test('falls back to the next endpoint when one is unreachable', () async {
      final (zns, urls) = mainnet((url) async => url == primary
          ? throw http.ClientException('connection refused', url)
          : http.Response(jsonEncode(okStatus), 200));
      expect((await zns.status()).syncedHeight, 1);
      expect(urls, znsNetworks[Network.mainnet]!.urls);
    });

    test('falls back on a server error', () async {
      final (zns, urls) = mainnet((url) async => url == primary
          ? http.Response('', 503, reasonPhrase: 'Unavailable')
          : http.Response(jsonEncode(okStatus), 200));
      expect((await zns.status()).syncedHeight, 1);
      expect(urls, hasLength(2));
    });

    test('does not fall back when the resolver answers with an RPC error',
        () async {
      final (zns, urls) = mainnet((_) async => http.Response(
          jsonEncode({
            'error': {'code': -1, 'message': 'nope'}
          }),
          200));
      await expectLater(
          zns.status(),
          throwsA(isA<ZnsRpcException>()
              .having((e) => e.message, 'message', 'nope')));
      expect(urls, hasLength(1));
    });

    test('does not fall back on a client error', () async {
      final (zns, urls) = mainnet(
          (_) async => http.Response('', 404, reasonPhrase: 'Not Found'));
      await expectLater(
          zns.status(),
          throwsA(isA<ZnsRpcException>()
              .having((e) => (e.code, e.isHttp), 'code', (404, true))));
      expect(urls, hasLength(1));
    });

    test('throws the last failure when every endpoint is unreachable',
        () async {
      final (zns, urls) = mainnet(
          (url) async => throw http.ClientException('connection refused', url));
      await expectLater(zns.status(), throwsA(isA<http.ClientException>()));
      expect(urls, hasLength(2));
    });

    test('does not fall back when an explicit url is set', () async {
      final urls = <Uri>[];
      final client = MockClient((request) async {
        urls.add(request.url);
        throw http.ClientException('connection refused', request.url);
      });
      final zns =
          ZNS(network: Network.mainnet, url: _resolver, httpClient: client);
      await expectLater(zns.status(), throwsA(isA<http.ClientException>()));
      expect(urls, [_resolver]);
    });
  });
}
