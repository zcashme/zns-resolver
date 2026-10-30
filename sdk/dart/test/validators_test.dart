import 'package:test/test.dart';
import 'package:zcashname_sdk/zcashname_sdk.dart';

const _testnetUa =
    'utest100qlkeru5c3m5kfrwe2hsmcfzmusreaza2prdyelg2kd2tr2842nceq952vay3gpmgky09fgft4z57h4z2zqzz5rcwgd4q90u54ek5yyca4s6e6y2jja9sww27kzedzznjcupcu0svq2exvq995c0lhl5zm53g4ksnm2xuwt3snv4dgh';
const _mainnetUa =
    'u1f68wxdxsrswjl04kcc397a9ljedg88rl8dgazrk8l9v8jx6t3gqsktvqyvmc3ypgf4pum03q4ftvy5s0fe803nrxm8kw6dgzlvnv83nv';

void main() {
  group('isValidName', () {
    test('accepts valid names', () {
      expect(isValidName('alice'), isTrue);
      expect(isValidName('bob123'), isTrue);
      expect(isValidName('a'), isTrue);
      expect(isValidName('a' * 63), isTrue);
    });

    test('rejects invalid names', () {
      expect(isValidName(''), isFalse);
      expect(isValidName('Alice'), isFalse);
      expect(isValidName('my-name'), isFalse);
      expect(isValidName('a' * 64), isFalse);
    });
  });

  group('normalizeName', () {
    test('trims and lowercases lookup input', () {
      expect(normalizeName('alice'), 'alice');
      expect(normalizeName('ALICE'), 'alice');
      expect(normalizeName('  Alice  '), 'alice');
    });

    test('does not strip suffixes or alter punctuation', () {
      expect(normalizeName('Alice.zcash'), 'alice.zcash');
      expect(normalizeName('alice.eth'), 'alice.eth');
      expect(normalizeName('alice.'), 'alice.');
    });

    test('lowercases ASCII letters only', () {
      // KELVIN SIGN lowercases to an ASCII k under full Unicode rules.
      expect(normalizeName('K'), 'K');
      expect(isValidName(normalizeName('K')), isFalse);
    });
  });

  group('isValidUnifiedAddress', () {
    test('accepts a valid testnet UA', () {
      expect(isValidUnifiedAddress(_testnetUa), isTrue);
      expect(
          isValidUnifiedAddress(znsNetworks[Network.testnet]!.treasuryAddress),
          isTrue);
    });

    test('accepts a valid mainnet UA', () {
      expect(isValidUnifiedAddress(_mainnetUa), isTrue);
    });

    test('rejects invalid addresses', () {
      expect(isValidUnifiedAddress(''), isFalse);
      expect(isValidUnifiedAddress('notanaddress'), isFalse);
      expect(isValidUnifiedAddress('zs1abc...'), isFalse);
      expect(isValidUnifiedAddress(_mainnetUa.toUpperCase()), isFalse);
      expect(isValidUnifiedAddress('u1${'q' * 104}'), isFalse);
    });

    test('rejects a mistyped character', () {
      final c = _mainnetUa[50] == 'q' ? 'p' : 'q';
      final typo = _mainnetUa.replaceRange(50, 51, c);
      expect(isValidUnifiedAddress(typo), isFalse);
    });
  });
}
