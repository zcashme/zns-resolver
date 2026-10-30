import 'package:http/http.dart' as http;

import 'exceptions.dart';
import 'models.dart';
import 'networks.dart';
import 'rpc.dart';
import 'validators.dart' as v;
import 'zip321.dart' as zip321;

/// Async client for the ZNS resolver JSON-RPC API.
class ZNS {
  /// The network preset in use.
  final Network network;

  /// The primary resolver JSON-RPC endpoint.
  final Uri url;

  final RpcClient _rpc;

  ZNS._(this.network, this._rpc) : url = _rpc.urls.first;

  /// Create a new ZNS client. Defaults to [Network.testnet], which fails
  /// safe.
  ///
  /// The client tries the endpoints of [network] in order. It tries the next
  /// endpoint only when one does not reply or replies with an HTTP 5xx
  /// error. If [url] is set, the client uses only that endpoint.
  ///
  /// [timeout] bounds each request to one endpoint. Defaults to 10 seconds.
  factory ZNS({
    Network network = Network.testnet,
    Uri? url,
    http.Client? httpClient,
    Duration? timeout,
  }) {
    final urls = url != null ? [url] : znsNetworks[network]!.urls;
    return ZNS._(
      network,
      RpcClient(urls, httpClient: httpClient, timeout: timeout),
    );
  }

  /// Close the underlying HTTP client. Only needed if you did not supply
  /// your own.
  void close() => _rpc.close();

  /// Fetch the resolver's sync state and registry identity.
  Future<Status> status() async {
    final raw = await _rpc.call<Map<String, dynamic>>('status');
    return Status.fromJson(raw);
  }

  /// Verify this client is talking to the ZNS registry for its network by
  /// comparing the server's viewing key against the key baked into this
  /// SDK. Call once at startup; on failure do not trust the server.
  ///
  /// Throws [ZnsVerificationException] on a mismatch. There are two
  /// possible causes: the endpoint is not the ZNS resolver, or this SDK
  /// predates a registry rotation.
  Future<void> verify() async {
    final expected = znsNetworks[network]!.viewingKey;
    final s = await status();
    if (s.viewingKey != expected) {
      throw ZnsVerificationException(
        "ZNS: the server's viewing key does not match the ZNS ${network.name} "
        'registry — either this is not the ZNS resolver, or this SDK '
        'predates a registry rotation. Upgrade zcashname_sdk.',
      );
    }
  }

  /// Resolve a ZNS name to its registration. Returns `null` if it is not
  /// registered.
  ///
  /// The name is trimmed and lowercased before querying, so `Alice` and
  /// `alice` resolve the same name. Returns `null` without hitting the
  /// server if the name is invalid after normalization. The returned
  /// registration's `name` is the normalized form, not the raw input.
  Future<Registration?> resolveName(String name) async {
    final normalized = v.normalizeName(name);
    if (!v.isValidName(normalized)) return null;
    final raw = await _rpc
        .call<Map<String, dynamic>?>('resolve', {'query': normalized});
    return raw == null ? null : Registration.fromJson(raw);
  }

  /// Resolve a unified address to all names pointing to it. Returns an empty
  /// list if there are none. Pagination via [limit] (default 50, max 500)
  /// and [offset] (default 0).
  Future<List<Registration>> resolveAddress(
    String address, {
    int? limit,
    int? offset,
  }) =>
      _resolveMany(address, limit, offset);

  /// Reverse lookup: all names currently bound to [address], with the total
  /// match count.
  Future<Paginated<Registration>> reverseLookup(
    String address, {
    int? limit,
    int? offset,
  }) async {
    final raw = await _rpc.call<Map<String, dynamic>>('reverse_lookup', {
      'address': address,
      if (limit != null) 'limit': limit,
      if (offset != null) 'offset': offset,
    });
    return Paginated.fromJson(raw, Registration.fromJson);
  }

  /// List all registered names. Same pagination as [resolveAddress].
  Future<List<Registration>> listAllRegistrations({int? limit, int? offset}) =>
      _resolveMany('', limit, offset);

  /// List all registered names, with the total match count.
  Future<Paginated<Registration>> listNames({int? limit, int? offset}) async {
    final raw = await _rpc.call<Map<String, dynamic>>('list_names', {
      if (limit != null) 'limit': limit,
      if (offset != null) 'offset': offset,
    });
    return Paginated.fromJson(raw, Registration.fromJson);
  }

  /// Check if [name] is available for registration. The name is normalized
  /// like [resolveName] first, so `isAvailable('Alice')` checks `alice`.
  /// Returns `false` immediately for a name that is invalid after
  /// normalization, without hitting the server.
  Future<bool> isAvailable(String name) async {
    final normalized = v.normalizeName(name);
    if (!v.isValidName(normalized)) return false;
    return await resolveName(normalized) == null;
  }

  /// Query the event log.
  Future<EventsResult> events([EventsFilter? filter]) async {
    final params = filter?.toJson() ?? const <String, dynamic>{};
    final raw = await _rpc.call<Map<String, dynamic>>('events', params);
    return EventsResult.fromJson(raw);
  }

  // ── Validation re-exports ────────────────────────────────────────────────

  bool isValidName(String name) => v.isValidName(name);
  String normalizeName(String name) => v.normalizeName(name);
  bool isValidUnifiedAddress(String address) =>
      v.isValidUnifiedAddress(address);

  /// Parse a ZIP-321 URI into its parts.
  zip321.Zip321Parts parseZip321Uri(String uri) => zip321.parseZip321Uri(uri);

  // ── Private ──────────────────────────────────────────────────────────────

  Future<List<Registration>> _resolveMany(
      String query, int? limit, int? offset) async {
    final raw = await _rpc.call<List<dynamic>>('resolve', {
      'query': query,
      if (limit != null) 'limit': limit,
      if (offset != null) 'offset': offset,
    });
    return raw
        .map((e) => Registration.fromJson(e as Map<String, dynamic>))
        .toList(growable: false);
  }
}
