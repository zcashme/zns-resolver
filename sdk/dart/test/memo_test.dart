import 'package:test/test.dart';
import 'package:zcashname_sdk/zcashname_sdk.dart';

// zns-mint/src/mint.rs test vector: a mainnet UA with Orchard, Sapling and
// transparent receivers.
const _ua =
    'u1d398kq0gfmegkvn0c57zmvq7gcnhxs6g3chfewlxq2yzhdjpx7uk3h80qgku5ygtyr9m7y6swgqe3pqdleu5uvwmangjj8yk7s5j0u78frtw9y9y5lx4c0x3cp054m9nl274xynwf5ad2uah7afyu4wgu3mwg5xvq4zmrdcplt8uqeqqw4vu4kdwngzvsn7gtdwtx3whkwt4z20pr0k';
// Live testnet treasury: Orchard and Sapling receivers.
const _testnetUa =
    'utest1tquw9k9qlsphhf30tau2qh758vfvf7xkvjea3rlc5mj4cjscgzfzgjeafjnl7r8tvv0akq3v2ze5q0azf4lnd4vf7kusrfensdsrr7tsgruvuqk0dv0hepnrk0pzgz3fj0mxc0lr29yf65nkwvw74gzep4792rkqa769lxl0sua09s0r';

Matcher _throwsFormat(Pattern message) => throwsA(isA<FormatException>()
    .having((e) => e.message, 'message', contains(message)));

void main() {
  group('mint request memos', () {
    test('builds the formats the mint parses', () {
      expect(buildClaimMemo(name: 'alice', ua: _ua, term: ZnsTerm.forever),
          'ZNS:claim:forever:alice:$_ua');
      expect(
          buildClaimMemo(name: 'alice', ua: _ua, term: const ZnsTerm.years(12)),
          'ZNS:claim:12y:alice:$_ua');
      expect(
          buildClaimMemo(
              name: 'alice',
              ua: _ua,
              term: ZnsTerm.forever,
              accessCode: '004206'),
          'ZNS:claim:004206:forever:alice:$_ua');
      expect(buildUpdateMemo(name: 'alice', ua: _ua),
          'ZNS:update:none:alice:$_ua');
      expect(buildUpdateMemo(name: 'alice', ua: _ua, term: ZnsTerm.forever),
          'ZNS:update:forever:alice:$_ua');
      expect(
          buildReleaseMemo(name: 'alice', ua: _ua), 'ZNS:release:alice:$_ua');
      expect(
          buildOtpMemo(OtpMemo(
              code: '417293',
              name: 'alice',
              action: OtpAction.update,
              ua: _ua)),
          'ZNS:otp:417293:alice:update:$_ua');
    });

    test('accepts a testnet UA', () {
      expect(buildReleaseMemo(name: 'alice', ua: _testnetUa),
          'ZNS:release:alice:$_testnetUa');
    });

    test('throws on input the mint refuses', () {
      String claim({
        String name = 'alice',
        String ua = _ua,
        ZnsTerm term = ZnsTerm.forever,
        String? accessCode,
      }) =>
          buildClaimMemo(
              name: name, ua: ua, term: term, accessCode: accessCode);

      expect(() => claim(accessCode: '4206'), _throwsFormat('six digits'));
      expect(() => claim(accessCode: 'a1b2c3'), _throwsFormat('six digits'));
      expect(() => claim(name: 'INVALID'), _throwsFormat('name'));
      for (final years in [0, 100, -1]) {
        expect(() => claim(term: ZnsTerm.years(years)), _throwsFormat('term'));
      }
      for (final ua in [
        _ua.substring(0, _ua.length - 1),
        '${_ua}q',
        _ua.toUpperCase(),
        '${_ua.substring(0, _ua.length - 1)}:',
        't1abc',
        'zs1abc',
        '',
      ]) {
        expect(() => claim(ua: ua), _throwsFormat('unified address'));
      }
      // One mistyped character, still in the bech32 alphabet: the checksum
      // fails.
      final typo = _ua.replaceRange(50, 51, _ua[50] == 'q' ? 'p' : 'q');
      expect(() => claim(ua: typo), _throwsFormat('unified address'));
      expect(
          () => buildOtpMemo(OtpMemo(
              code: '41729', name: 'alice', action: OtpAction.update, ua: _ua)),
          _throwsFormat('six digits'));
    });
  });

  group('parseOtpMemo', () {
    const challenge = OtpMemo(
        code: '417293', name: 'alice', action: OtpAction.release, ua: _ua);

    test('reads back what buildOtpMemo writes, ignoring memo padding', () {
      final text = buildOtpMemo(challenge);
      expect(parseOtpMemo(text), challenge);
      expect(parseOtpMemo(text + '\x00' * (512 - text.length)), challenge);
    });

    test('returns null for anything else', () {
      expect(parseOtpMemo('ZNS:claim:forever:alice:$_ua'), isNull);
      expect(parseOtpMemo('ZNS:otp:417293:alice:claim:$_ua'), isNull);
      expect(parseOtpMemo('ZNS:otp:417293:alice:update'), isNull);
      expect(parseOtpMemo('hello world'), isNull);
    });
  });
}
