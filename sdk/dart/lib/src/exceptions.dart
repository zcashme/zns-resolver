/// Thrown by [ZNS.verify] when the server's viewing key is not the ZNS
/// registry key for the client's network.
class ZnsVerificationException implements Exception {
  final String message;
  const ZnsVerificationException(this.message);

  @override
  String toString() => 'ZnsVerificationException: $message';
}
