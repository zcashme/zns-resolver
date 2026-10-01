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
  synced: boolean;
  viewingKey: string;
  registered: number;
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
