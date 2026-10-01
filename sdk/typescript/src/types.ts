/** Read-only data types returned by the ZNS resolver. */

export type Zats = number;
export type Network = "testnet" | "mainnet";

export const ZNS_ACTIONS = ["CLAIM", "BUY", "UPDATE", "LIST", "DELIST", "RELEASE"] as const;
export type ZnsAction = (typeof ZNS_ACTIONS)[number];
export type LastAction = Exclude<ZnsAction, "LIST">;
export type EventAction = ZnsAction | "SETPRICE";

export interface PendingBuy {
  buyer: string;
  price: Zats;
  claimHeight: number;
  expiresAt: number;
  txid: string;
}

export interface Listing {
  name: string;
  price: Zats;
  payTaddr: string;
  nonce: number;
  txid: string;
  height: number;
  pendingBuy: PendingBuy | undefined;
}

export interface Registration {
  name: string;
  address: string;
  txid: string;
  height: number;
  nonce: number;
  lastAction: LastAction;
  listing: Listing | null;
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

export interface Pricing {
  nonce: number;
  height: number;
  tiers: Zats[];
}

export interface Status {
  syncedHeight: number;
  uivk: string;
  address: string;
  registered: number;
  listed: number;
  pricing: Pricing | null;
}

export interface Event {
  id: number;
  name: string;
  action: EventAction;
  txid: string;
  height: number;
  ua: string | null;
  price: Zats | null;
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
