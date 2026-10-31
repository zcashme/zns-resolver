/// The bech32m checksum (BIP 350), enough to check a unified address.
/// `package:bech32` implements only the original bech32 constant.
library;

const String _charset = 'qpzry9x8gf2tvdw0s3jn54khce6mua7l';
const int _bech32mConst = 0x2bc830a3;
const List<int> _generator = [
  0x3b6a57b2,
  0x26508e6d,
  0x1ea119fa,
  0x3d4233dd,
  0x2a1462b3,
];

int _polymod(Iterable<int> values) {
  var chk = 1;
  for (final v in values) {
    final top = chk >> 25;
    chk = (chk & 0x1ffffff) << 5 ^ v;
    for (var i = 0; i < 5; i++) {
      if ((top >> i) & 1 == 1) chk ^= _generator[i];
    }
  }
  return chk;
}

/// The human-readable part of a lowercase bech32m string, or `null` if the
/// string is not bech32m: a bad character, a missing separator, or a
/// checksum that fails.
String? bech32mHrp(String s) {
  final sep = s.lastIndexOf('1');
  if (sep < 1 || sep + 7 > s.length) return null;
  final hrp = s.substring(0, sep);
  if (hrp.codeUnits.any((c) => c < 33 || c > 126)) return null;
  final data = <int>[];
  for (final c in s.substring(sep + 1).split('')) {
    final v = _charset.indexOf(c);
    if (v < 0) return null;
    data.add(v);
  }
  final expanded = [
    ...hrp.codeUnits.map((c) => c >> 5),
    0,
    ...hrp.codeUnits.map((c) => c & 31),
  ];
  return _polymod([...expanded, ...data]) == _bech32mConst ? hrp : null;
}
