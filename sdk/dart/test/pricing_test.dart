import 'package:test/test.dart';
import 'package:zcashname_sdk/zcashname_sdk.dart';

void main() {
  test("matches the mint's pricing law", () {
    expect(znsAnnualUsdTiers, {1: 10000, 2: 2500, 3: 800, 4: 400, 5: 100});
    expect(znsAnnualUsdMinimum, 20);
    expect(znsForeverMultiple, 3);
  });

  group('annualUsd', () {
    test('prices every tier', () {
      expect(annualUsd('a'), 10000);
      expect(annualUsd('ab'), 2500);
      expect(annualUsd('abc'), 800);
      expect(annualUsd('abcd'), 400);
      expect(annualUsd('abcde'), 100);
      expect(annualUsd('abcdef'), 20); // first minimum tier
      expect(annualUsd('a' * 63), 20); // max length
    });

    test('clamps empty input to the 1-char tier', () {
      expect(annualUsd(''), 10000);
    });
  });

  test('foreverUsd charges three annual prices, one time', () {
    expect(foreverUsd('bob'), 2400);
    expect(foreverUsd('alice'), 300);
    expect(foreverUsd('a' * 6), 60);
  });

  group('quoteZats', () {
    test('converts USD at the given rate', () {
      expect(quoteZats(800, 1000), 800000);
    });

    test('rounds up so quotes never undercut the price', () {
      expect(quoteZats(20, 333), 6660);
      expect(quoteZats(1, 0.5), 1);
    });
  });
}
