import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter } from "react-router";
import { describe, expect, it, vi } from "vitest";

import { ps5SettingsLink, settingsConsoleId, settingsTabOf } from "./settingsTabs";

vi.mock("../../lib/tauriEnv", () => ({ isTauriEnv: () => true, safeUnlisten: () => {} }));
vi.mock("../../state/lang", async (orig) => ({
  ...(await orig<typeof import("../../state/lang")>()),
  useTr: () =>
    (key: string, vars?: unknown, fallback?: string) =>
      typeof vars === "string" ? vars : (fallback ?? key),
}));

// The screens read the stores' live state, so the roster set below is what they show.
vi.mock("zustand", async (orig) => {
  const { liveZustand } = await import("../../test-utils/liveZustand");
  return liveZustand(await orig<typeof import("zustand")>());
});

const { default: SettingsScreen } = await import("./index");
const { useRosterStore } = await import("../../state/roster");
type Profiles = ReturnType<typeof useRosterStore.getState>["profiles"];

const ROSTER: Profiles = [
  { id: "lr", name: "Living room", host: "192.168.1.50", settings: { keep_awake_mode: "always", rest_after_upload: true } },
  { id: "bd", name: "Bedroom", host: "192.168.1.51", settings: { keep_awake_mode: "off" } },
];
useRosterStore.setState({ profiles: ROSTER, active_id: "lr" });

function render(path: string): string {
  return renderToStaticMarkup(
    <MemoryRouter initialEntries={[path]}>
      <SettingsScreen />
    </MemoryRouter>,
  );
}

/** The state of the switch whose label is `label` (the switch comes before its label). */
const switchBefore = (html: string, label: string) => {
  const at = html.indexOf(label);
  const before = html.slice(0, at);
  return /aria-checked="(true|false)"/g.exec(before.slice(before.lastIndexOf('role="switch"')))?.[1];
};

/** The `<option>` the picker has selected. */
const pickedConsole = (html: string) =>
  /data-testid="settings-ps5-picker"[^>]*>[\s\S]*?<option value="([^"]+)" selected=""/.exec(html)?.[1];

describe("settings tabs: addresses", () => {
  it("opens App unless the address asks for This PS5", () => {
    expect(settingsTabOf(new URLSearchParams(""))).toBe("app");
    expect(settingsTabOf(new URLSearchParams("tab=ps5"))).toBe("ps5");
    expect(settingsTabOf(new URLSearchParams("tab=other"))).toBe("app");
  });

  it("picks the console the address names, else the selected one, else the first", () => {
    const list = [{ id: "a" }, { id: "b" }];
    expect(settingsConsoleId("b", list, "a")).toBe("b");
    expect(settingsConsoleId("gone", list, "a")).toBe("a");
    expect(settingsConsoleId(null, list, null)).toBe("a");
    expect(settingsConsoleId(null, [], "a")).toBeNull();
  });

  it("links to This PS5 for one console", () => {
    expect(ps5SettingsLink("lr")).toBe("/settings?tab=ps5&console=lr");
    expect(ps5SettingsLink(null)).toBe("/settings?tab=ps5");
  });
});

describe("Settings screen: App and This PS5", () => {
  it("App holds this device and app-wide choices, not one console's", () => {
    const html = render("/settings");
    expect(html).toContain('role="tablist"');
    expect(html).toContain(">App<");
    expect(html).toContain(">This PS5<");
    expect(html).toContain('data-testid="settings-app"');
    expect(html).not.toContain('data-testid="settings-ps5"');
    for (const here of ["Language", "Always overwrite without asking", "Keep Minion off on consoles I add", "Tailscale on this device"]) {
      expect(html).toContain(here);
    }
    for (const gone of [
      "Keep the PS5 awake",
      "Put the PS5 in rest mode after uploads finish",
      "Wake the PS5 for a waiting upload",
      "Install by file path when the PS5 cannot reach this computer",
      "Bring up automatically",
      "Guests play as",
    ]) {
      expect(html).not.toContain(gone);
    }
  });

  it("/settings#tailscale still lands on App, where this device's Tailscale is", () => {
    const html = render("/settings#tailscale");
    expect(html).toContain('data-testid="settings-app"');
    expect(html).toContain("Tailscale on this device");
  });

  it("This PS5 has a picker on the selected console, and that console's settings", () => {
    const html = render("/settings?tab=ps5");
    expect(html).toContain('data-testid="settings-ps5"');
    expect(html).toContain('data-testid="settings-ps5-picker"');
    expect(html).toContain("Living room (192.168.1.50)");
    expect(html).toContain("Bedroom (192.168.1.51)");
    expect(pickedConsole(html)).toBe("lr");
    for (const here of [
      "Bring up automatically",
      "Reconnect automatically after rest mode",
      "Keep the PS5 awake",
      "Put the PS5 in rest mode after uploads finish",
      "Wake the PS5 for a waiting upload",
      "Install by file path when the PS5 cannot reach this computer",
      "Fan threshold",
    ]) {
      expect(html).toContain(here);
    }
    // Living room keeps awake "Always" and rests after uploads.
    expect(html).toMatch(/<option value="always" selected="">/);
    expect(switchBefore(html, "Put the PS5 in rest mode after uploads finish")).toBe("true");
    // Not set on Living room: the default (on).
    expect(switchBefore(html, "Wake the PS5 for a waiting upload")).toBe("true");
  });

  it("each console shows its own values", () => {
    const html = render("/settings?tab=ps5&console=bd");
    expect(switchBefore(html, "Put the PS5 in rest mode after uploads finish")).toBe("false");
  });

  it("?console= opens This PS5 on that console", () => {
    const html = render("/settings?tab=ps5&console=bd");
    expect(pickedConsole(html)).toBe("bd");
    expect(html).toMatch(/<option value="off" selected="">/);
  });

  it("an unknown ?console= falls back to the selected console", () => {
    expect(pickedConsole(render("/settings?tab=ps5&console=nope"))).toBe("lr");
  });

  it("says each per-PS5 hint is about this PS5, not every console", () => {
    const html = render("/settings?tab=ps5");
    const tab = html.slice(html.indexOf('data-testid="settings-ps5"'));
    expect(tab).toContain("this PS5");
    expect(tab).not.toMatch(/every console|any console|a saved PS5/i);
  });

  it("with no consoles, This PS5 points at Connection", () => {
    useRosterStore.setState({ profiles: [], active_id: null });
    try {
      const html = render("/settings?tab=ps5");
      expect(html).toContain("No PS5 yet");
      expect(html).not.toContain('data-testid="settings-ps5-picker"');
    } finally {
      useRosterStore.setState({ profiles: ROSTER, active_id: "lr" });
    }
  });
});
