/// Read-only data types returned by the ZNS resolver. Hand-rolled
/// `fromJson`, mirroring the snake_case shape of the resolver's records.
library;

import 'actions.dart';

ZnsAction _action(Object? wire) {
  final action = wire is String ? ZnsAction.fromWire(wire) : null;
  if (action == null) {
    throw FormatException('ZNS: unknown action from the resolver', wire);
  }
  return action;
}

/// The current binding of a registered name.
class Registration {
  final String name;

  /// The unified address the name resolves to.
  final String address;

  /// The transaction that produced the current binding.
  final String txid;

  /// Block height of [txid].
  final int height;

  /// The action that produced the current binding.
  final ZnsAction lastAction;

  /// `"none"` or a decimal Unix timestamp.
  final String expiresAt;

  const Registration({
    required this.name,
    required this.address,
    required this.txid,
    required this.height,
    required this.lastAction,
    required this.expiresAt,
  });

  factory Registration.fromJson(Map<String, dynamic> json) => Registration(
        name: json['name'] as String,
        address: json['address'] as String,
        txid: json['txid'] as String,
        height: json['height'] as int,
        lastAction: _action(json['last_action']),
        expiresAt: json['expires_at'] as String,
      );

  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      other is Registration &&
          other.name == name &&
          other.address == address &&
          other.txid == txid &&
          other.height == height &&
          other.lastAction == lastAction &&
          other.expiresAt == expiresAt;

  @override
  int get hashCode =>
      Object.hash(name, address, txid, height, lastAction, expiresAt);

  @override
  String toString() =>
      'Registration(name: $name, address: $address, lastAction: $lastAction, '
      'expiresAt: $expiresAt)';
}

/// The resolver's sync state and registry identity.
class Status {
  final int syncedHeight;

  /// Whether the resolver has reached the chain tip. `null` from a resolver
  /// that does not report it.
  final bool? synced;

  /// Registry viewing key.
  final String viewingKey;

  /// Treasury payment address, published by the mint.
  final String? treasuryAddress;

  const Status({
    required this.syncedHeight,
    required this.viewingKey,
    this.synced,
    this.treasuryAddress,
  });

  factory Status.fromJson(Map<String, dynamic> json) => Status(
        syncedHeight: (json['synced_height'] as int?) ?? 0,
        synced: json['synced'] as bool?,
        viewingKey: json['viewing_key'] as String,
        treasuryAddress: json['treasury_address'] as String?,
      );

  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      other is Status &&
          other.syncedHeight == syncedHeight &&
          other.synced == synced &&
          other.viewingKey == viewingKey &&
          other.treasuryAddress == treasuryAddress;

  @override
  int get hashCode =>
      Object.hash(syncedHeight, synced, viewingKey, treasuryAddress);

  @override
  String toString() => 'Status(syncedHeight: $syncedHeight, synced: $synced)';
}

/// One page of results from `list_names` or `reverse_lookup`.
class Paginated<T> {
  final List<T> items;

  /// Matches across all pages.
  final int total;

  /// The limit the resolver applied.
  final int limit;

  /// The offset the resolver applied.
  final int offset;

  const Paginated({
    required this.items,
    required this.total,
    required this.limit,
    required this.offset,
  });

  factory Paginated.fromJson(
    Map<String, dynamic> json,
    T Function(Map<String, dynamic>) item,
  ) =>
      Paginated(
        items: (json['items'] as List<dynamic>)
            .map((e) => item(e as Map<String, dynamic>))
            .toList(growable: false),
        total: json['total'] as int,
        limit: json['limit'] as int,
        offset: json['offset'] as int,
      );

  @override
  String toString() =>
      'Paginated(total: $total, limit: $limit, offset: $offset, '
      'items: ${items.length})';
}

/// One entry in the resolver's append-only event log.
class Event {
  final int id;
  final String name;
  final ZnsAction action;
  final String txid;
  final int height;

  /// Position of the action within its block.
  final int actionIndex;

  /// The address in this action's Name Note.
  final String address;

  /// `"none"` or a decimal Unix timestamp.
  final String expiresAt;

  const Event({
    required this.id,
    required this.name,
    required this.action,
    required this.txid,
    required this.height,
    required this.actionIndex,
    required this.address,
    required this.expiresAt,
  });

  factory Event.fromJson(Map<String, dynamic> json) => Event(
        id: json['id'] as int,
        name: json['name'] as String,
        action: _action(json['action']),
        txid: json['txid'] as String,
        height: json['height'] as int,
        actionIndex: json['action_index'] as int,
        address: json['address'] as String,
        expiresAt: json['expires_at'] as String,
      );

  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      other is Event &&
          other.id == id &&
          other.name == name &&
          other.action == action &&
          other.txid == txid &&
          other.height == height &&
          other.actionIndex == actionIndex &&
          other.address == address &&
          other.expiresAt == expiresAt;

  @override
  int get hashCode => Object.hash(
      id, name, action, txid, height, actionIndex, address, expiresAt);

  @override
  String toString() =>
      'Event(id: $id, name: $name, action: $action, height: $height)';
}

/// Filter for [ZNS.events]. Every field is optional.
class EventsFilter {
  final String? name;
  final ZnsAction? action;
  final int? sinceHeight;
  final int? limit;
  final int? offset;

  const EventsFilter({
    this.name,
    this.action,
    this.sinceHeight,
    this.limit,
    this.offset,
  });

  /// The `events` params, with the resolver's parameter names.
  Map<String, dynamic> toJson() => {
        if (name != null) 'name': name,
        if (action != null) 'action': action!.wire,
        if (sinceHeight != null) 'since_height': sinceHeight,
        if (limit != null) 'limit': limit,
        if (offset != null) 'offset': offset,
      };
}

/// One page of the event log.
class EventsResult {
  final List<Event> events;

  /// Matching events across all pages.
  final int total;
  final int limit;
  final int offset;

  const EventsResult({
    required this.events,
    required this.total,
    required this.limit,
    required this.offset,
  });

  factory EventsResult.fromJson(Map<String, dynamic> json) => EventsResult(
        events: (json['events'] as List<dynamic>)
            .map((e) => Event.fromJson(e as Map<String, dynamic>))
            .toList(growable: false),
        total: json['total'] as int,
        limit: json['limit'] as int,
        offset: json['offset'] as int,
      );

  @override
  String toString() =>
      'EventsResult(total: $total, limit: $limit, offset: $offset, '
      'events: ${events.length})';
}
