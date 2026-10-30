import 'dart:convert';

/// ZIP-321 limits: the largest payment amount in ZEC, and the memo size.
const int _maxMoney = 21000000;
const int _maxMemoBytes = 512;

final RegExp _amountRegExp = RegExp(r'^\d+(\.\d{1,8})?$');
final RegExp _addressStop = RegExp(r'[?&:\s]');

/// Parsed components of a `zcash:` ZIP-321 payment URI.
class Zip321Parts {
  final String address;
  final String amount;
  final String memoRaw;
  final String memoDecoded;

  const Zip321Parts({
    required this.address,
    required this.amount,
    required this.memoRaw,
    required this.memoDecoded,
  });

  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      other is Zip321Parts &&
          other.address == address &&
          other.amount == amount &&
          other.memoRaw == memoRaw &&
          other.memoDecoded == memoDecoded;

  @override
  int get hashCode => Object.hash(address, amount, memoRaw, memoDecoded);

  @override
  String toString() =>
      'Zip321Parts(address: $address, amount: $amount, memo: $memoDecoded)';
}

/// Base64url-encode a UTF-8 string with no padding.
String toBase64Url(String text) =>
    base64Url.encode(utf8.encode(text)).replaceAll('=', '');

/// Decode a base64url string, with or without padding, back to UTF-8.
/// Returns an empty string on any decode failure.
String decodeBase64Url(String value) {
  try {
    return utf8.decode(base64Url.decode(base64Url.normalize(value)));
  } on FormatException {
    return '';
  }
}

/// ZIP-321 payment URI: `zcash:<address>[?amount=<ZEC>][&memo=<base64url>]`.
/// [amount] is a decimal ZEC string with at most 8 decimal places. Throws a
/// [FormatException] for input a wallet would refuse.
String buildZip321Uri({
  required String address,
  String? amount,
  String? memo,
}) {
  if (address.isEmpty || _addressStop.hasMatch(address)) {
    throw FormatException('ZNS: a payment URI needs an address.', address);
  }
  final params = <String>[];
  if (amount != null) {
    if (!_amountRegExp.hasMatch(amount)) {
      throw FormatException(
        'ZNS: the amount is a decimal ZEC value with at most 8 decimal places.',
        amount,
      );
    }
    // Compare the decimal string itself, not a floating-point conversion.
    final [whole, ...rest] = amount.split('.');
    final fraction = rest.isEmpty ? '' : rest.first;
    final wholeZec = BigInt.parse(whole);
    final max = BigInt.from(_maxMoney);
    if (wholeZec > max ||
        (wholeZec == max && fraction.contains(RegExp('[1-9]')))) {
      throw FormatException(
          'ZNS: the amount is more than $_maxMoney ZEC.', amount);
    }
    params.add('amount=$amount');
  }
  if (memo != null && memo.isNotEmpty) {
    if (address.startsWith('t')) {
      throw FormatException(
          'ZNS: a transparent address cannot receive a memo.', address);
    }
    if (utf8.encode(memo).length > _maxMemoBytes) {
      throw FormatException(
          'ZNS: the memo is more than $_maxMemoBytes bytes.', memo);
    }
    params.add('memo=${toBase64Url(memo)}');
  }
  return params.isEmpty
      ? 'zcash:$address'
      : 'zcash:$address?${params.join('&')}';
}

/// Parse a `zcash:` URI into its parts. The scheme match is
/// case-insensitive; missing fields yield empty strings.
Zip321Parts parseZip321Uri(String uri) {
  var withoutScheme = uri;
  if (withoutScheme.toLowerCase().startsWith('zcash:')) {
    withoutScheme = withoutScheme.substring(6);
  }

  final qIdx = withoutScheme.indexOf('?');
  final addressPart =
      qIdx == -1 ? withoutScheme : withoutScheme.substring(0, qIdx);
  final query = qIdx == -1 ? '' : withoutScheme.substring(qIdx + 1);

  Map<String, String> params;
  try {
    params = Uri.splitQueryString(query);
  } on ArgumentError {
    // Bad percent-encoding: read it as a URI without fields.
    params = const {};
  }
  final amount = (params['amount'] ?? '').trim();
  final memoRaw = (params['memo'] ?? '').trim();
  final memoDecoded = memoRaw.isEmpty ? '' : decodeBase64Url(memoRaw);

  return Zip321Parts(
    address: addressPart.trim(),
    amount: amount,
    memoRaw: memoRaw,
    memoDecoded: memoDecoded,
  );
}
