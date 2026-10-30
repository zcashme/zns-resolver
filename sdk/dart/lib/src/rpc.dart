import 'dart:async';
import 'dart:convert';

import 'package:http/http.dart' as http;

/// Thrown when a resolver answers with an error: an HTTP error status, or a
/// JSON-RPC envelope `error`.
class ZnsRpcException implements Exception {
  /// JSON-RPC error code when [isHttp] is false; the HTTP status code (e.g.
  /// 404, 500) when [isHttp] is true.
  final int code;
  final String message;
  final bool isHttp;

  ZnsRpcException(this.code, this.message, {this.isHttp = false});

  /// A server error may be one node's alone, so the next endpoint is tried.
  bool get _isRetryable => isHttp && code >= 500;

  @override
  String toString() => isHttp
      ? 'ZnsRpcException: ZNS HTTP $code: $message'
      : 'ZnsRpcException: ZNS RPC error $code: $message';
}

/// Internal JSON-RPC 2.0 client. Not exported in the public surface.
class RpcClient {
  /// Endpoints, tried in order.
  final List<Uri> urls;
  final http.Client httpClient;
  final Duration timeout;
  int _id = 0;

  /// Whether [httpClient] is owned (and therefore closed by [close]).
  final bool _ownsClient;

  RpcClient(this.urls, {http.Client? httpClient, Duration? timeout})
      : assert(urls.isNotEmpty),
        httpClient = httpClient ?? http.Client(),
        _ownsClient = httpClient == null,
        timeout = timeout ?? const Duration(seconds: 10);

  Future<T> call<T>(String method, [Map<String, dynamic>? params]) async {
    final id = ++_id;
    final body = jsonEncode({
      'jsonrpc': '2.0',
      'id': id,
      'method': method,
      'params': params ?? <String, dynamic>{},
    });

    // Endpoints are tried in order. Only a failure to get an answer moves on
    // to the next one; a resolver's own reply, even an error, is final.
    Object? lastError;
    StackTrace? lastStack;
    for (final url in urls) {
      try {
        return await _post<T>(url, body);
      } on ZnsRpcException catch (e, st) {
        if (!e._isRetryable) rethrow;
        lastError = e;
        lastStack = st;
      } on Exception catch (e, st) {
        lastError = e;
        lastStack = st;
      }
    }
    Error.throwWithStackTrace(lastError!, lastStack!);
  }

  Future<T> _post<T>(Uri url, String body) async {
    final response = await httpClient
        .post(
          url,
          headers: const {'Content-Type': 'application/json'},
          body: body,
        )
        .timeout(timeout);

    if (response.statusCode < 200 || response.statusCode >= 300) {
      throw ZnsRpcException(
        response.statusCode,
        response.reasonPhrase ?? 'HTTP error',
        isHttp: true,
      );
    }

    final decoded = jsonDecode(response.body) as Map<String, dynamic>;
    final error = decoded['error'];
    if (error != null) {
      final m = error as Map<String, dynamic>;
      throw ZnsRpcException(
        (m['code'] as int?) ?? -32603,
        (m['message'] as String?) ?? 'Unknown error',
      );
    }
    return decoded['result'] as T;
  }

  void close() {
    if (_ownsClient) httpClient.close();
  }
}
