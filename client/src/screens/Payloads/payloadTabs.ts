export type PayloadTabId = "catalog" | "send" | "ps5" | "running";

/** Payloads first, then what is running on the console: its processes and the ports in use. */
const ALL_TABS: readonly PayloadTabId[] = ["catalog", "send", "ps5", "running"];

/** Every build has every tab: the catalogue runs on the engine in a browser (`/api/payloads`), and
 *  browser Send forwards selected ELF bytes through it. */
export function payloadTabs(): readonly PayloadTabId[] {
  return ALL_TABS;
}

/** The tab a bare /payloads opens on: what is on the console once one is connected,
 *  the catalogue before that. */
export function defaultPayloadTab(connected: boolean): PayloadTabId {
  return connected ? "ps5" : "catalog";
}

/** The card id each retired per-integration tab now opens on "On your PS5". */
const LEGACY_CARD: Record<string, string> = {
  manager: "payload-manager",
  shadowmount: "shadowmountplus",
  nanodns: "nanodns",
  hearbridge: "hearbridge",
};

/** Where an old Payloads tab (`?tab=manager|shadowmount|nanodns|hearbridge|processes|ports`)
 *  lives now, or null for a current tab. Integrations became cards on "On your PS5";
 *  Processes and Ports became one Running tab. */
export function legacyPayloadsTarget(tab: string | null): string | null {
  if (!tab) return null;
  if (tab === "processes" || tab === "ports") return RUNNING_LINK;
  const card = LEGACY_CARD[tab];
  return card ? payloadCardLink(card) : null;
}

/** The Running tab: the console's processes, then the ports in use. */
export const RUNNING_LINK = "/payloads?tab=running";

/** The link that opens "On your PS5" with one card's Manage panel expanded. */
export function payloadCardLink(cardId: string): string {
  return `/payloads?tab=ps5&open=${encodeURIComponent(cardId)}`;
}
