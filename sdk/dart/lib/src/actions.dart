/// The actions a Name Note records: `claim`, `update` and `release`.
///
/// Each value carries the lowercase wire string the resolver uses in
/// `last_action`, `action` and the `events` filter.
enum ZnsAction {
  claim('claim'),
  update('update'),
  release('release');

  const ZnsAction(this.wire);

  /// The action as the resolver writes it.
  final String wire;

  /// Parse a wire string. Returns `null` for anything else.
  static ZnsAction? fromWire(String s) {
    for (final a in values) {
      if (a.wire == s) return a;
    }
    return null;
  }

  @override
  String toString() => wire;
}
