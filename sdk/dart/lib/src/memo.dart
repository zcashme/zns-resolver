/// Mint request memos.
///
///     Claim:                   ZNS:claim:<term>:<name>:<ua>
///     Claim a protected name:  ZNS:claim:<access code>:<term>:<name>:<ua>
///     Update:                  ZNS:update:<term or none>:<name>:<ua>
///     Release:                 ZNS:release:<name>:<ua>
///     OTP reply:               ZNS:otp:<otp code>:<name>:<update or release>:<ua>
///
/// Each builder throws a [FormatException] for input the mint refuses.
library;

import 'validators.dart';

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
enum OtpAction { update, release }

/// The fields of an OTP memo.
class OtpMemo {
  final String code;
  final String name;
  final OtpAction action;
  final String ua;

  const OtpMemo({
    required this.code,
    required this.name,
    required this.action,
    required this.ua,
  });

  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      other is OtpMemo &&
          other.code == code &&
          other.name == name &&
          other.action == action &&
          other.ua == ua;

  @override
  int get hashCode => Object.hash(code, name, action, ua);

  @override
  String toString() =>
      'OtpMemo(code: $code, name: $name, action: ${action.name}, ua: $ua)';
}

const int _maxTermYears = 99;
final RegExp _sixDigits = RegExp(r'^\d{6}$');
final RegExp _trailingNuls = RegExp('\x00+\$');

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

/// OTP memo: `ZNS:otp:<otp code>:<name>:<update or release>:<ua>`. The mint
/// sends it as the challenge; the wallet sends the same text back.
String buildOtpMemo(OtpMemo memo) {
  if (!_sixDigits.hasMatch(memo.code)) {
    throw FormatException('ZNS: the OTP code is six digits.', memo.code);
  }
  _checkName(memo.name);
  _checkUa(memo.ua);
  return 'ZNS:otp:${memo.code}:${memo.name}:${memo.action.name}:${memo.ua}';
}

/// Read a mint challenge. Returns `null` for anything that is not an OTP
/// memo. Trailing NUL padding from a 512-byte memo field is ignored.
OtpMemo? parseOtpMemo(String text) {
  final parts = text.replaceFirst(_trailingNuls, '').split(':');
  if (parts.length != 6 || parts[0] != 'ZNS' || parts[1] != 'otp') {
    return null;
  }
  final [_, _, code, name, actionField, ua] = parts;
  final action = switch (actionField) {
    'update' => OtpAction.update,
    'release' => OtpAction.release,
    _ => null,
  };
  if (action == null ||
      !_sixDigits.hasMatch(code) ||
      !isValidName(name) ||
      !isValidUnifiedAddress(ua)) {
    return null;
  }
  return OtpMemo(code: code, name: name, action: action, ua: ua);
}
