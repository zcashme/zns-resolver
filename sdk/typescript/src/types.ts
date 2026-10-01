/** Read-only data types returned by the ZNS resolver. */

export type Network = "testnet" | "mainnet";

export const ZNS_ACTIONS = ["CLAIM", "UPDATE", "RELEASE"] as const;
export type ZnsAction = (typeof ZNS_ACTIONS)[number];
export type LastAction = ZnsAction;
export type EventAction = ZnsAction;

export interface Registration {
  name: string;
  address: string;
  txid: string;
  height: number;
  nonce: number;
  lastAction: LastAction;
}

export interface MerkleProof {
  index: number;
  path: string[];
  root: string;
  height: number;
  leafCount: number;
}

export interface RegistrationWithProof extends Registration {
  proof: MerkleProof;
}

export interface Status {
  syncedHeight: number;
  uivk: string;
  address: string;
  registered: number;
}

export interface Event {
  id: number;
  name: string;
  action: EventAction;
  txid: string;
  height: number;
  ua: string | null;
  nonce: number | null;
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
}
