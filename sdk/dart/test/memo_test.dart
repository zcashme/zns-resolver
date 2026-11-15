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

  group('marketplace memos', () {
    // A mainnet public-key-hash address: where a buyer pays.
    const t1 = 't1Hsc1LR8yKnbbe3twRp88p6vFfC5t7DLbs';
    const auction = DutchAsk(
        startZats: 500000000,
        minZats: 150000000,
        startBlock: 3600000,
        endBlock: 3610000);

    test('builds the formats the mint parses', () {
      expect(
          buildListMemo(
              name: 'alice', ua: _ua, payTo: t1, priceZats: 250000000),
          'ZNS:list:alice:$_ua:$t1:2.5');
      expect(buildListMemo(name: 'alice', ua: _ua, payTo: t1, priceZats: 1),
          'ZNS:list:alice:$_ua:$t1:0.00000001');
      expect(
          buildAuctionMemo(name: 'alice', ua: _ua, payTo: t1, auction: auction),
          'ZNS:auction:alice:$_ua:$t1:5:1.5:3600000:3610000');
      expect(buildDelistMemo(name: 'alice', ua: _ua), 'ZNS:delist:alice:$_ua');
      expect(buildBuyMemo(name: 'alice', ua: _ua), 'ZNS:buy:alice:$_ua');
    });

    test('throws on terms the mint refuses', () {
      String list({String payTo = t1, int priceZats = 1}) => buildListMemo(
          name: 'alice', ua: _ua, payTo: payTo, priceZats: priceZats);
      expect(() => list(payTo: _ua), _throwsFormat('t1'));
      // A testnet address for a mainnet name.
      expect(() => list(payTo: 'tmEZhbWHTpdKMw5it8YDspUXSMGQyFwovpU'),
          _throwsFormat('t1'));
      for (final priceZats in [-1, 21000001 * 100000000]) {
        expect(() => list(priceZats: priceZats), _throwsFormat('zatoshis'));
      }
      String auctionWith({int? endBlock, int? minZats}) => buildAuctionMemo(
          name: 'alice',
          ua: _ua,
          payTo: t1,
          auction: DutchAsk(
              startZats: auction.startZats,
              minZats: minZats ?? auction.minZats,
              startBlock: auction.startBlock,
              endBlock: endBlock ?? auction.endBlock));
      expect(() => auctionWith(endBlock: auction.startBlock),
          _throwsFormat('ends after'));
      expect(() => auctionWith(minZats: auction.startZats),
          _throwsFormat('falls'));
    });

    test('reads back list and auction challenges with their terms', () {
      const list = OtpMemo(
          code: '417293',
          name: 'alice',
          action: OtpAction.list,
          ua: _ua,
          terms: ListingTerms(payTo: t1, ask: FlatAsk(250000000)));
      expect(buildOtpMemo(list), 'ZNS:otp:417293:alice:list:$_ua:$t1:2.5');
      expect(parseOtpMemo(buildOtpMemo(list)), list);
      const dutch = OtpMemo(
          code: '417293',
          name: 'alice',
          action: OtpAction.auction,
          ua: _ua,
          terms: ListingTerms(payTo: t1, ask: auction));
      expect(parseOtpMemo(buildOtpMemo(dutch)), dutch);
      expect(
          parseOtpMemo('ZNS:otp:417293:alice:delist:$_ua'),
          const OtpMemo(
              code: '417293',
              name: 'alice',
              action: OtpAction.delist,
              ua: _ua));
    });

    test('refuses a challenge whose terms do not fit its verb', () {
      for (final text in [
        'ZNS:otp:417293:alice:list:$_ua',
        'ZNS:otp:417293:alice:list:$_ua:$t1:5:1.5:3600000:3610000',
        'ZNS:otp:417293:alice:auction:$_ua:$t1:1.5:5:3600000:3610000',
        'ZNS:otp:417293:alice:delist:$_ua:$t1:2.5',
        'ZNS:otp:417293:alice:list:$_ua:$t1:02.5',
      ]) {
        expect(parseOtpMemo(text), isNull, reason: text);
      }
      expect(
          () => buildOtpMemo(const OtpMemo(
              code: '417293',
              name: 'alice',
              action: OtpAction.update,
              ua: _ua,
              terms: ListingTerms(payTo: t1, ask: FlatAsk(1)))),
          _throwsFormat('terms'));
    });

    test("reads the mint's answer to a buy", () {
      expect(parseBuyOtpMemo('ZNS:buyotp:004206:alice:$_ua'),
          const BuyOtpMemo(code: '004206', name: 'alice', ua: _ua));
      expect(
          parseBuyOtpMemo('ZNS:dutch:004206:alice:$_ua:3.25:1763215200'
              '${'\x00' * 8}'),
          const BuyOtpMemo(
              code: '004206',
              name: 'alice',
              ua: _ua,
              dutch: DutchQuote(priceZats: 325000000, until: 1763215200)));
      expect(parseBuyOtpMemo('ZNS:otp:004206:alice:update:$_ua'), isNull);
      expect(parseBuyOtpMemo('ZNS:buyotp:004206:alice:$_ua:3.25'), isNull);
      expect(parseBuyOtpMemo('ZNS:dutch:004206:alice:$_ua:3.25'), isNull);
    });

    test('pays the price plus the buy code in zatoshis', () {
      expect(buyPaymentZats(250000000, '004206'), 250004206);
      expect(
          () => buyPaymentZats(250000000, '4206'), _throwsFormat('six digits'));
    });
  });
}
