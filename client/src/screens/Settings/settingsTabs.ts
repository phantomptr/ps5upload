/** Settings has two tabs: App (this device and app-wide) and This PS5 (one console's own settings).
 *  `/settings` and `/settings#tailscale` open App; `/settings?tab=ps5&console=<profile id>` opens
 *  This PS5 on that console (the selected one when the id is left out or unknown). */
export type SettingsTab = "app" | "ps5";

export function settingsTabOf(params: URLSearchParams): SettingsTab {
  return params.get("tab") === "ps5" ? "ps5" : "app";
}

/** The console the This PS5 tab shows: the one the address names when it is in the roster, else
 *  the selected console, else the first. Null with no consoles. */
export function settingsConsoleId(
  requested: string | null,
  profiles: readonly { id: string }[],
  activeId: string | null,
): string | null {
  if (requested && profiles.some((p) => p.id === requested)) return requested;
  if (activeId && profiles.some((p) => p.id === activeId)) return activeId;
  return profiles[0]?.id ?? null;
}

/** The address of This PS5 settings for one console (roster profile id). */
export function ps5SettingsLink(consoleId?: string | null): string {
  const p = new URLSearchParams({ tab: "ps5" });
  if (consoleId) p.set("console", consoleId);
  return `/settings?${p}`;
}
