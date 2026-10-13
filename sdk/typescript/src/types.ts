/** Read-only data types returned by the ZNS resolver. */

export const ZNS_ACTIONS = ["claim", "update", "release"] as const;
export type ZnsAction = (typeof ZNS_ACTIONS)[number];
export type LastAction = ZnsAction;
export type EventAction = ZnsAction;

export interface Registration {
  name: string;
  address: string;
  txid: string;
  height: number;
  lastAction: LastAction;
  expiresAt: string;
}

export interface Status {
  syncedHeight: number;
  synced?: boolean;
  /** Registry viewing key. */
  viewingKey: string;
  /** Legacy mainnet indexer pricing: admin-set zats tiers indexed by name
   *  length, the last tier is the catchall. Only the legacy mainnet indexer
   *  serves pricing today. */
  pricing?: Pricing;
  registered: number;
}

/** Legacy mainnet indexer pricing: admin-set zats tiers indexed by name
 *  length, the last tier is the catchall for longer names. The current
 *  (Name Note) resolver does not serve pricing. */
export interface Pricing {
  nonce: number;
  height: number;
  tiers: number[];
}

/** Paginated result envelope from the typed list verbs (`list_names`,
 *  `reverse_lookup`). Mirrors the resolver's `Paginated<T>`. */
export interface Paginated<T> {
  items: T[];
  total: number;
  limit: number;
  offset: number;
}

export interface Event {
  id: number;
  name: string;
  action: EventAction;
  txid: string;
  height: number;
  actionIndex: number;
  address: string;
  expiresAt: string;
}

export interface EventsFilter {
  name?: string;
  action?: EventAction;
  sinceHeight?: number;
  limit?: number;
  offset?: number;
}

export interface EventsResult {
  events: Event[];
  total: number;
  limit: number;
  offset: number;
}
