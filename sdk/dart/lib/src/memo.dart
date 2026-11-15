/// Mint request memos.
///
///     Claim:                   ZNS:claim:<term>:<name>:<ua>
///     Claim a protected name:  ZNS:claim:<access code>:<term>:<name>:<ua>
///     Update:                  ZNS:update:<term or none>:<name>:<ua>
///     Release:                 ZNS:release:<name>:<ua>
///     List at a price:         ZNS:list:<name>:<ua>:<t1>:<price>
///     List as an auction:      ZNS:auction:<name>:<ua>:<t1>:<start>:<min>:<start block>:<end block>
///     Delist:                  ZNS:delist:<name>:<ua>
///     Buy:                     ZNS:buy:<name>:<buyer ua>
///     OTP reply:               ZNS:otp:<otp code>:<name>:<action>:<ua>[:<t1>:<terms>]
///
/// The mint answers a buy with `ZNS:buyotp:<code>:<name>:<buyer ua>`, or for
/// an auction `ZNS:dutch:<code>:<name>:<buyer ua>:<price>:<until>`. The buyer
/// pays the price plus the code, read as zatoshis, to the listing's t1.
///
/// Each builder throws a [FormatException] for input the mint refuses.
library;

import 'validators.dart';
import 'zip321.dart' show maxMoneyZec;

/// A registration term: 1-99 whole years, or forever.
sealed class ZnsTerm {
  const ZnsTerm();

  /// A term of [count] whole years, 1 to 99.
  const factory ZnsTerm.years(int count) = ZnsYears;

  /// A registration that does not expire.
  static const ZnsTerm forever = ZnsForever._();
}

final class ZnsYears extends ZnsTerm {
  final int count;
  const ZnsYears(this.count);

  @override
  bool operator ==(Object other) => other is ZnsYears && other.count == count;

  @override
  int get hashCode => count.hashCode;

  @override
  String toString() => '${count}y';
}

final class ZnsForever extends ZnsTerm {
  const ZnsForever._();

  @override
  String toString() => 'forever';
}

/// Actions the mint challenges with an OTP. Claims are never challenged.
enum OtpAction { update, release, list, auction, delist }

/// What a buyer pays. Amounts are zatoshis.
sealed class ListingAsk {
  const ListingAsk();
}

/// A fixed price.
final class FlatAsk extends ListingAsk {
  final int priceZats;
  const FlatAsk(this.priceZats);

  @override
  bool operator ==(Object other) =>
      other is FlatAsk && other.priceZats == priceZats;

  @override
  int get hashCode => priceZats.hashCode;

  @override
  String toString() => 'FlatAsk($priceZats)';
}

/// An auction: the price falls from [startZats] at [startBlock] to
/// [minZats] at [endBlock].
final class DutchAsk extends ListingAsk {
  final int startZats;
  final int minZats;
  final int startBlock;
  final int endBlock;
  const DutchAsk({
    required this.startZats,
    required this.minZats,
    required this.startBlock,
    required this.endBlock,
  });

  @override
  bool operator ==(Object other) =>
      other is DutchAsk &&
      other.startZats == startZats &&
      other.minZats == minZats &&
      other.startBlock == startBlock &&
      other.endBlock == endBlock;

  @override
  int get hashCode => Object.hash(startZats, minZats, startBlock, endBlock);

  @override
  String toString() =>
      'DutchAsk($startZats → $minZats, blocks $startBlock–$endBlock)';
}

/// A listing's terms: the transparent address the buyer pays, and the ask.
class ListingTerms {
  final String payTo;
  final ListingAsk ask;
  const ListingTerms({required this.payTo, required this.ask});

  @override
  bool operator ==(Object other) =>
      other is ListingTerms && other.payTo == payTo && other.ask == ask;

  @override
  int get hashCode => Object.hash(payTo, ask);

  @override
  String toString() => 'ListingTerms(payTo: $payTo, ask: $ask)';
}

/// The fields of an OTP memo.
class OtpMemo {
  final String code;
  final String name;
  final OtpAction action;
  final String ua;

  /// Present exactly when [action] is `list` (a [FlatAsk]) or `auction` (a
  /// [DutchAsk]).
  final ListingTerms? terms;

  const OtpMemo({
    required this.code,
    required this.name,
    required this.action,
    required this.ua,
    this.terms,
  });

  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      other is OtpMemo &&
          other.code == code &&
          other.name == name &&
          other.action == action &&
          other.ua == ua &&
          other.terms == terms;

  @override
  int get hashCode => Object.hash(code, name, action, ua, terms);

  @override
  String toString() => 'OtpMemo(code: $code, name: $name, '
      'action: ${action.name}, ua: $ua, terms: $terms)';
}

/// The price an auction buyer's block fixed, and the unix time the quote
/// holds until.
class DutchQuote {
  final int priceZats;
  final int until;
  const DutchQuote({required this.priceZats, required this.until});

  @override
  bool operator ==(Object other) =>
      other is DutchQuote &&
      other.priceZats == priceZats &&
      other.until == until;

  @override
  int get hashCode => Object.hash(priceZats, until);

  @override
  String toString() => 'DutchQuote($priceZats until $until)';
}

/// The mint's answer to a buy. [dutch] is set for an auction.
class BuyOtpMemo {
  final String code;
  final String name;
  final String ua;
  final DutchQuote? dutch;
  const BuyOtpMemo({
    required this.code,
    required this.name,
    required this.ua,
    this.dutch,
  });

  @override
  bool operator ==(Object other) =>
      other is BuyOtpMemo &&
      other.code == code &&
      other.name == name &&
      other.ua == ua &&
      other.dutch == dutch;

  @override
  int get hashCode => Object.hash(code, name, ua, dutch);

  @override
  String toString() =>
      'BuyOtpMemo(code: $code, name: $name, ua: $ua, dutch: $dutch)';
}

const int _maxTermYears = 99;
final RegExp _sixDigits = RegExp(r'^\d{6}$');
final RegExp _trailingNuls = RegExp('\x00+\$');

const int _zatsPerZec = 100000000;
const int _maxZats = maxMoneyZec * _zatsPerZec;
const int _maxHeight = 0xffffffff;
final RegExp _base58Address = RegExp(r'^[1-9A-HJ-NP-Za-km-z]{35}$');
final RegExp _zec = RegExp(r'^(0|[1-9]\d*)(\.\d{1,8})?$');
final RegExp _decimal = RegExp(r'^(0|[1-9]\d*)$');

bool _isZats(int zats) => zats >= 0 && zats <= _maxZats;

/// Zatoshis as the mint writes ZEC: whole zatoshis stay an integer, and a
/// fraction drops trailing zeros (`5`, `1.5`, `0.00000001`).
String _encodeZec(int zats) {
  if (!_isZats(zats)) {
    throw FormatException(
        'ZNS: an amount is whole zatoshis within the money supply.', zats);
  }
  final whole = zats ~/ _zatsPerZec;
  final frac = zats % _zatsPerZec;
  if (frac == 0) return '$whole';
  final digits =
      frac.toString().padLeft(8, '0').replaceFirst(RegExp(r'0+$'), '');
  return '$whole.$digits';
}

/// ZEC text as the mint reads it, in zatoshis, or `null`.
int? _decodeZec(String text) {
  if (!_zec.hasMatch(text)) return null;
  final [whole, ...rest] = text.split('.');
  final frac = rest.isEmpty ? '' : rest.first;
  if (whole.length > 8) return null;
  final zats =
      int.parse(whole) * _zatsPerZec + int.parse(frac.padRight(8, '0'));
  return _isZats(zats) ? zats : null;
}

bool _isHeight(int height) => height >= 0 && height <= _maxHeight;

int? _decodeHeight(String text) {
  if (!_decimal.hasMatch(text) || text.length > 10) return null;
  final height = int.parse(text);
  return _isHeight(height) ? height : null;
}

/// The largest whole number a JavaScript number holds exactly, 2^53 - 1: the
/// TypeScript SDK's bound on a dutch quote's expiry.
const int _maxUntil = 9007199254740991;

/// A dutch quote's expiry in unix seconds, or `null`.
int? _decodeUntil(String text) {
  if (!_decimal.hasMatch(text) || text.length > 16) return null;
  final until = int.parse(text);
  return until <= _maxUntil ? until : null;
}

/// The shape of a transparent public-key-hash address on the UA's network:
/// `t1` on mainnet, `tm` on testnet, 35 base58 characters. The checksum is
/// not verified; the mint refuses a bad one and keeps the listing fee.
bool _isPayTo(String address, String ua) =>
    _base58Address.hasMatch(address) &&
    address.startsWith(ua.startsWith('utest1') ? 'tm' : 't1');

bool _isValidAuction(DutchAsk ask) =>
    ask.endBlock > ask.startBlock && ask.startZats > ask.minZats;

/// `<t1>:<price>`, or `<t1>:<start>:<min>:<start block>:<end block>`.
String _termsFields(ListingTerms terms, String ua) {
  if (!_isPayTo(terms.payTo, ua)) {
    throw FormatException(
        "ZNS: pay to a transparent t1 address on the name's network.",
        terms.payTo);
  }
  switch (terms.ask) {
    case FlatAsk(:final priceZats):
      return '${terms.payTo}:${_encodeZec(priceZats)}';
    case final DutchAsk ask:
      if (!_isHeight(ask.startBlock) || !_isHeight(ask.endBlock)) {
        throw FormatException("ZNS: an auction's blocks are heights.", ask);
      }
      if (!_isValidAuction(ask)) {
        throw FormatException(
            'ZNS: an auction ends after it starts, and its price falls.', ask);
      }
      return '${terms.payTo}:${_encodeZec(ask.startZats)}:'
          '${_encodeZec(ask.minZats)}:${ask.startBlock}:${ask.endBlock}';
  }
}

/// Reads `<t1>:<terms>` back. [fields] are the parts after the UA.
ListingTerms? _parseTermsFields(
    List<String> fields, OtpAction action, String ua) {
  if (fields.isEmpty || !_isPayTo(fields.first, ua)) return null;
  final [payTo, ...rest] = fields;
  switch ((action, rest)) {
    case (OtpAction.list, [final price]):
      final priceZats = _decodeZec(price);
      return priceZats == null
          ? null
          : ListingTerms(payTo: payTo, ask: FlatAsk(priceZats));
    case (OtpAction.auction, [final start, final min, final from, final to]):
      final startZats = _decodeZec(start);
      final minZats = _decodeZec(min);
      final startBlock = _decodeHeight(from);
      final endBlock = _decodeHeight(to);
      if (startZats == null ||
          minZats == null ||
          startBlock == null ||
          endBlock == null) {
        return null;
      }
      final ask = DutchAsk(
          startZats: startZats,
          minZats: minZats,
          startBlock: startBlock,
          endBlock: endBlock);
      return _isValidAuction(ask) ? ListingTerms(payTo: payTo, ask: ask) : null;
    default:
      return null;
  }
}

String _termField(ZnsTerm term) => switch (term) {
      ZnsForever() => 'forever',
      ZnsYears(:final count) when count >= 1 && count <= _maxTermYears =>
        '${count}y',
      ZnsYears() =>
        throw FormatException('ZNS: a term is 1-99 years, or forever.', term),
    };

void _checkName(String name) {
  if (!isValidName(name)) {
    throw FormatException(
      'ZNS: a name is 1-63 characters: lowercase letters and digits only.',
      name,
    );
  }
}

void _checkUa(String ua) {
  if (!isValidUnifiedAddress(ua)) {
    throw FormatException('ZNS: not a unified address the mint accepts.', ua);
  }
}

/// Claim request: `ZNS:claim:<term>:<name>:<ua>`. To claim a protected
/// name, set [accessCode]: `ZNS:claim:<access code>:<term>:<name>:<ua>`.
String buildClaimMemo({
  required String name,
  required String ua,
  required ZnsTerm term,
  String? accessCode,
}) {
  if (accessCode != null && !_sixDigits.hasMatch(accessCode)) {
    throw FormatException('ZNS: the access code is six digits.', accessCode);
  }
  final field = _termField(term);
  _checkName(name);
  _checkUa(ua);
  return accessCode == null
      ? 'ZNS:claim:$field:$name:$ua'
      : 'ZNS:claim:$accessCode:$field:$name:$ua';
}

/// Update request: `ZNS:update:<term or none>:<name>:<ua>`. Without a
/// [term] the current expiry carries forward.
String buildUpdateMemo({
  required String name,
  required String ua,
  ZnsTerm? term,
}) {
  final field = term == null ? 'none' : _termField(term);
  _checkName(name);
  _checkUa(ua);
  return 'ZNS:update:$field:$name:$ua';
}

/// Release request: `ZNS:release:<name>:<ua>`.
String buildReleaseMemo({required String name, required String ua}) {
  _checkName(name);
  _checkUa(ua);
  return 'ZNS:release:$name:$ua';
}

/// List request: `ZNS:list:<name>:<ua>:<t1>:<price>`. [ua] is the name's
/// current address; a buyer pays [priceZats] to [payTo].
String buildListMemo({
  required String name,
  required String ua,
  required String payTo,
  required int priceZats,
}) {
  _checkName(name);
  _checkUa(ua);
  final terms = ListingTerms(payTo: payTo, ask: FlatAsk(priceZats));
  return 'ZNS:list:$name:$ua:${_termsFields(terms, ua)}';
}

/// Auction request:
/// `ZNS:auction:<name>:<ua>:<t1>:<start>:<min>:<start block>:<end block>`.
String buildAuctionMemo({
  required String name,
  required String ua,
  required String payTo,
  required DutchAsk auction,
}) {
  _checkName(name);
  _checkUa(ua);
  final terms = ListingTerms(payTo: payTo, ask: auction);
  return 'ZNS:auction:$name:$ua:${_termsFields(terms, ua)}';
}

/// Delist request: `ZNS:delist:<name>:<ua>`.
String buildDelistMemo({required String name, required String ua}) {
  _checkName(name);
  _checkUa(ua);
  return 'ZNS:delist:$name:$ua';
}

/// Buy request: `ZNS:buy:<name>:<ua>`. [ua] is the buyer's address, the one
/// the sale binds the name to.
String buildBuyMemo({required String name, required String ua}) {
  _checkName(name);
  _checkUa(ua);
  return 'ZNS:buy:$name:$ua';
}

/// OTP memo: `ZNS:otp:<otp code>:<name>:<action>:<ua>`, with `:<t1>:<terms>`
/// after the UA for a list or an auction. The mint sends it as the
/// challenge; the wallet sends the same text back.
String buildOtpMemo(OtpMemo memo) {
  if (!_sixDigits.hasMatch(memo.code)) {
    throw FormatException('ZNS: the OTP code is six digits.', memo.code);
  }
  _checkName(memo.name);
  _checkUa(memo.ua);
  final head =
      'ZNS:otp:${memo.code}:${memo.name}:${memo.action.name}:${memo.ua}';
  return switch ((memo.action, memo.terms)) {
    (OtpAction.update || OtpAction.release || OtpAction.delist, null) => head,
    (OtpAction.list, final ListingTerms terms) when terms.ask is FlatAsk =>
      '$head:${_termsFields(terms, memo.ua)}',
    (OtpAction.auction, final ListingTerms terms) when terms.ask is DutchAsk =>
      '$head:${_termsFields(terms, memo.ua)}',
    _ => throw FormatException(
        'ZNS: a list or auction challenge carries its terms, and no other does.',
        memo),
  };
}

/// Read a mint challenge. Returns `null` for anything that is not an OTP
/// memo. Trailing NUL padding from a 512-byte memo field is ignored.
OtpMemo? parseOtpMemo(String text) {
  final parts = text.replaceFirst(_trailingNuls, '').split(':');
  if (parts.length < 6 || parts[0] != 'ZNS' || parts[1] != 'otp') {
    return null;
  }
  final [_, _, code, name, actionField, ua, ...rest] = parts;
  final action = OtpAction.values.asNameMap()[actionField];
  if (action == null ||
      !_sixDigits.hasMatch(code) ||
      !isValidName(name) ||
      !isValidUnifiedAddress(ua)) {
    return null;
  }
  if (action == OtpAction.list || action == OtpAction.auction) {
    final terms = _parseTermsFields(rest, action, ua);
    return terms == null
        ? null
        : OtpMemo(code: code, name: name, action: action, ua: ua, terms: terms);
  }
  return rest.isEmpty
      ? OtpMemo(code: code, name: name, action: action, ua: ua)
      : null;
}

/// Read the mint's answer to a buy: `ZNS:buyotp:<code>:<name>:<ua>`, or
/// `ZNS:dutch:<code>:<name>:<ua>:<price>:<until>` for an auction. Returns
/// `null` for anything else. Trailing NUL padding is ignored.
BuyOtpMemo? parseBuyOtpMemo(String text) {
  final parts = text.replaceFirst(_trailingNuls, '').split(':');
  final (code, name, ua, dutch) = switch (parts) {
    ['ZNS', 'buyotp', final code, final name, final ua] => (
        code,
        name,
        ua,
        null
      ),
    ['ZNS', 'dutch', final code, final name, final ua, final price, final until]
        when _decodeZec(price) != null && _decodeUntil(until) != null =>
      (
        code,
        name,
        ua,
        DutchQuote(priceZats: _decodeZec(price)!, until: _decodeUntil(until)!)
      ),
    _ => (null, null, null, null),
  };
  if (code == null ||
      name == null ||
      ua == null ||
      !_sixDigits.hasMatch(code) ||
      !isValidName(name) ||
      !isValidUnifiedAddress(ua)) {
    return null;
  }
  return BuyOtpMemo(code: code, name: name, ua: ua, dutch: dutch);
}

/// What a buyer pays to the listing's t1: the price plus the buy code, the
/// code read as zatoshis (`004206` adds 4,206).
int buyPaymentZats(int priceZats, String code) {
  if (!_sixDigits.hasMatch(code)) {
    throw FormatException('ZNS: the buy code is six digits.', code);
  }
  final zats = priceZats + int.parse(code);
  if (!_isZats(priceZats) || !_isZats(zats)) {
    throw FormatException(
        'ZNS: an amount is whole zatoshis within the money supply.', zats);
  }
  return zats;
}
