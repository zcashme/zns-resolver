import 'package:test/test.dart';
import 'package:zcashname_sdk/zcashname_sdk.dart';

void main() {
  test("matches the mint's pricing law", () {
    expect(znsAnnualUsdTiers, {
      1: 12480,
      2: 2720,
      3: 920,
      4: 472,
      5: 188,
      6: 112,
      7: 48,
      8: 32,
      9: 24,
    });
    expect(znsAnnualUsdMinimum, 18);
    expect(znsForeverMultiple, 4);
  });

  group('annualUsd', () {
    test('prices every tier', () {
      const tiers = [12480, 2720, 920, 472, 188, 112, 48, 32, 24];
      for (var i = 0; i < tiers.length; i++) {
        expect(annualUsd('a' * (i + 1)), tiers[i]);
      }
      expect(annualUsd('a' * 10), 18); // first minimum tier
      expect(annualUsd('a' * 63), 18); // max length
    });

    test('clamps empty input to the 1-char tier', () {
      expect(annualUsd(''), 12480);
    });
  });

  test('foreverUsd charges four annual prices, one time', () {
    expect(foreverUsd('bob'), 3680);
    expect(foreverUsd('alice'), 752);
    expect(foreverUsd('a' * 10), 72);
  });

  group('quoteZats', () {
    test('converts USD at the given rate', () {
      expect(quoteZats(920, 1000), 920000);
    });

    test('rounds up so quotes never undercut the price', () {
      expect(quoteZats(18, 333), 5994);
      expect(quoteZats(1, 0.5), 1);
    });
  });
}
