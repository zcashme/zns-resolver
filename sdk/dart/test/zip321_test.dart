import 'package:test/test.dart';
import 'package:zcashname_sdk/zcashname_sdk.dart';

const _ua =
    'u1d398kq0gfmegkvn0c57zmvq7gcnhxs6g3chfewlxq2yzhdjpx7uk3h80qgku5ygtyr9m7y6swgqe3pqdleu5uvwmangjj8yk7s5j0u78frtw9y9y5lx4c0x3cp054m9nl274xynwf5ad2uah7afyu4wgu3mwg5xvq4zmrdcplt8uqeqqw4vu4kdwngzvsn7gtdwtx3whkwt4z20pr0k';
const _testnetUa =
    'utest1tquw9k9qlsphhf30tau2qh758vfvf7xkvjea3rlc5mj4cjscgzfzgjeafjnl7r8tvv0akq3v2ze5q0azf4lnd4vf7kusrfensdsrr7tsgruvuqk0dv0hepnrk0pzgz3fj0mxc0lr29yf65nkwvw74gzep4792rkqa769lxl0sua09s0r';

Matcher _throwsFormat(Pattern message) => throwsA(isA<FormatException>()
    .having((e) => e.message, 'message', contains(message)));

void main() {
  group('buildZip321Uri', () {
    test('builds a URI that parseZip321Uri reads back', () {
      final memo =
          buildClaimMemo(name: 'alice', ua: _ua, term: ZnsTerm.forever);
      final uri =
          buildZip321Uri(address: _testnetUa, amount: '0.0123', memo: memo);
      expect(uri, isNot(endsWith('=')));
      expect(
        parseZip321Uri(uri),
        Zip321Parts(
          address: _testnetUa,
          amount: '0.0123',
          memoRaw: uri.split('memo=')[1],
          memoDecoded: memo,
        ),
      );
    });

    test('rejects an invalid amount', () {
      for (final amount in ['', '1.123456789', '-1', '1e3']) {
        expect(() => buildZip321Uri(address: _testnetUa, amount: amount),
            _throwsFormat('amount'));
      }
    });

    test('rejects an amount above 21,000,000 ZEC', () {
      expect(buildZip321Uri(address: _testnetUa, amount: '21000000'),
          'zcash:$_testnetUa?amount=21000000');
      expect(buildZip321Uri(address: _testnetUa, amount: '21000000.00000000'),
          'zcash:$_testnetUa?amount=21000000.00000000');
      for (final amount in [
        '21000001',
        '21000000.00000001',
        '99999999999999999999',
      ]) {
        expect(() => buildZip321Uri(address: _testnetUa, amount: amount),
            _throwsFormat('21000000 ZEC'));
      }
    });

    test('rejects a memo longer than 512 UTF-8 bytes', () {
      expect(buildZip321Uri(address: _testnetUa, memo: 'a' * 512), isNotEmpty);
      expect(() => buildZip321Uri(address: _testnetUa, memo: 'a' * 513),
          _throwsFormat('512 bytes'));
      // 257 two-byte characters: 257 characters, 514 bytes.
      expect(() => buildZip321Uri(address: _testnetUa, memo: 'é' * 257),
          _throwsFormat('512 bytes'));
    });

    test('rejects a memo for a transparent address', () {
      const taddr = 't1Hsc1LR8yKnbbe3twRp88p6vFfC5t7DLbs';
      expect(
          buildZip321Uri(address: taddr, amount: '1'), 'zcash:$taddr?amount=1');
      expect(() => buildZip321Uri(address: taddr, memo: 'hi'),
          _throwsFormat('transparent'));
    });

    test('rejects a missing or malformed address', () {
      for (final address in ['', 'u1abc?amount=1', 'zcash:u1abc', 'u1 abc']) {
        expect(
            () => buildZip321Uri(address: address), _throwsFormat('address'));
      }
    });
  });

  group('parseZip321Uri', () {
    test('parses address and amount', () {
      final result = parseZip321Uri('zcash:u1abc?amount=1.5&memo=abc123');
      expect(result.address, 'u1abc');
      expect(result.amount, '1.5');
    });

    test('decodes a base64url memo', () {
      expect(parseZip321Uri('zcash:u1abc?memo=SGVsbG8').memoDecoded, 'Hello');
    });

    test('returns empty strings for missing fields', () {
      final result = parseZip321Uri('zcash:u1abc');
      expect(result.amount, '');
      expect(result.memoRaw, '');
      expect(result.memoDecoded, '');
    });

    test('handles the zcash: prefix case-insensitively', () {
      expect(parseZip321Uri('ZCASH:u1abc').address, 'u1abc');
    });

    test('decodes an undecodable memo to an empty string', () {
      expect(parseZip321Uri('zcash:u1abc?memo=*').memoDecoded, '');
    });
  });
}
