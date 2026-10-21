export type PayloadTabId = "catalog" | "send" | "ps5";

const ALL_TABS: readonly PayloadTabId[] = ["catalog", "send", "ps5"];

/** Catalog downloads use the native shell; browser Send forwards selected ELF bytes through the engine. */
export function payloadTabsFor(native: boolean): readonly PayloadTabId[] {
  return native ? ALL_TABS : ALL_TABS.filter((t) => t !== "catalog");
}

/** The tab a bare /payloads opens on: what is on the console once one is connected,
 *  the catalogue before that. */
export function defaultPayloadTab(native: boolean, connected: boolean): PayloadTabId {
  if (!native || connected) return "ps5";
  return "catalog";
}

/** The card id each retired per-integration tab now opens on "On your PS5". */
const LEGACY_CARD: Record<string, string> = {
  manager: "payload-manager",
  shadowmount: "shadowmountplus",
  nanodns: "nanodns",
  hearbridge: "hearbridge",
};

/** Where an old Payloads tab (`?tab=manager|shadowmount|nanodns|ports`) lives now, or null
 *  for a current tab. Integrations became cards on "On your PS5"; Ports moved next to
 *  Processes. */
export function legacyPayloadsTarget(tab: string | null): string | null {
  if (!tab) return null;
  if (tab === "ports") return "/processes?tab=ports";
  const card = LEGACY_CARD[tab];
  return card ? payloadCardLink(card) : null;
}

/** The link that opens "On your PS5" with one card's Manage panel expanded. */
export function payloadCardLink(cardId: string): string {
  return `/payloads?tab=ps5&open=${encodeURIComponent(cardId)}`;
}
