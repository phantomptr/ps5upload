export type PayloadTabId = "catalog" | "send" | "shadowmount" | "nanodns";

const ALL_TABS: readonly PayloadTabId[] = ["catalog", "send", "shadowmount", "nanodns"];

/**
 * The Payloads tabs this build can use.
 *
 * Catalog and Send download ELFs to this machine and write them to the
 * console's loader socket, which the browser build cannot do (payload_send and
 * payloads_* have no browser mapping). ShadowMount+ and nanoDNS only read and
 * write files on the console through the engine (smp_status, fs_read_preview,
 * fs_write_bytes_run), so the web UI keeps those two.
 */
export function payloadTabsFor(native: boolean): readonly PayloadTabId[] {
  return native ? ALL_TABS : ALL_TABS.filter((t) => t === "shadowmount" || t === "nanodns");
}
