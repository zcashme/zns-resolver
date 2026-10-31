import 'bech32m.dart';

final RegExp _nameRegExp = RegExp(r'^[a-z0-9]{1,63}$');

/// Validate a ZNS name: 1-63 lowercase letters and digits.
bool isValidName(String name) => _nameRegExp.hasMatch(name);

final RegExp _upperAscii = RegExp('[A-Z]');

/// Canonicalize lookup input: trim whitespace and lowercase ASCII letters.
/// It does not strip a `.zcash` or `.zec` suffix.
String normalizeName(String name) =>
    name.trim().replaceAllMapped(_upperAscii, (m) => m[0]!.toLowerCase());

/// Lengths of a mainnet UA the mint accepts: an Orchard receiver, optionally
/// Sapling and/or transparent, nothing else. Testnet adds 4 (`utest` vs `u`).
const List<int> _mainnetUaLengths = [106, 141, 178, 213];
final List<int> _testnetUaLengths =
    _mainnetUaLengths.map((n) => n + 4).toList(growable: false);

/// Validate a Zcash unified address by length and bech32m checksum. It does
/// not decode the receivers, so it cannot see whether an Orchard receiver is
/// present: a Sapling-only UA has the same length as an Orchard-only one.
bool isValidUnifiedAddress(String address) {
  final lengths =
      address.startsWith('utest1') ? _testnetUaLengths : _mainnetUaLengths;
  if (!lengths.contains(address.length) || address != address.toLowerCase()) {
    return false;
  }
  final hrp = bech32mHrp(address);
  return hrp == 'u' || hrp == 'utest';
}
