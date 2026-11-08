import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter } from "react-router";
import { describe, expect, it, vi } from "vitest";
import { settingsVisibility } from "./visibility";

const platform = { tauri: true, mobile: false };
vi.mock("../../lib/tauriEnv", () => ({
  isTauriEnv: () => platform.tauri,
  safeUnlisten: () => {},
}));
vi.mock("../../lib/platform", async (orig) => ({
  ...(await orig<typeof import("../../lib/platform")>()),
  isMobile: () => platform.mobile,
  isAndroid: () => platform.mobile,
}));
vi.mock("../../state/lang", async (orig) => ({
  ...(await orig<typeof import("../../state/lang")>()),
  useTr: () =>
    (key: string, vars?: unknown, fallback?: string) =>
      typeof vars === "string" ? vars : (fallback ?? key),
}));

// The screens read the stores' live state, so the console set below shows in the This PS5 tab.
vi.mock("zustand", async (orig) => {
  const { liveZustand } = await import("../../test-utils/liveZustand");
  return liveZustand(await orig<typeof import("zustand")>());
});

const { default: SettingsScreen } = await import("./index");
const { useRosterStore } = await import("../../state/roster");
useRosterStore.setState({ profiles: [{ id: "p1", name: "Living room", host: "192.168.1.50" }], active_id: "p1" });

function render(p: { tauri: boolean; mobile: boolean }, path = "/settings"): string {
  platform.tauri = p.tauri;
  platform.mobile = p.mobile;
  return renderToStaticMarkup(
    <MemoryRouter initialEntries={[path]}>
      <SettingsScreen />
    </MemoryRouter>,
  );
}

describe("settingsVisibility", () => {
  it("shows everything on the desktop app", () => {
    const { engineBuiltIn, ...rest } = settingsVisibility({ tauri: true, mobile: false });
    expect(engineBuiltIn).toBe(false);
    expect(Object.values(rest)).not.toContain(false);
  });

  it("hides the engine URL on the phone apps, whose engine is built in", () => {
    const v = settingsVisibility({ tauri: true, mobile: true });
    expect(v.engineUrl).toBe(false);
    expect(v.engineBuiltIn).toBe(true);
    expect(v.keepDeviceAwake).toBe(true);
    expect(v.appUpdates).toBe(true);
  });

  it("hides what the browser build cannot do, and keeps the screen-on switch", () => {
    expect(settingsVisibility({ tauri: false, mobile: false })).toEqual({
      engineUrl: false,
      engineBuiltIn: false,
      keepDeviceAwake: true,
      settingsFile: false,
      osNotifications: false,
      reconnectAfterRest: false,
      appUpdates: false,
    });
  });
});

describe("Settings screen per platform", () => {
  it("a phone says why there is no Engine URL", () => {
    const html = render({ tauri: true, mobile: true });
    expect(html).toContain("built-in engine");
    expect(html).not.toContain("Engine URL");
  });

  it("desktop shows the engine URL, keep-awake, settings file, OS notifications and updates", () => {
    const html = render({ tauri: true, mobile: false });
    expect(html).toContain("Engine URL");
    expect(html).toContain("Keep computer awake");
    expect(html).toContain("Settings file");
    expect(html).toContain("Show system notifications");
    expect(html).toContain("Check for updates automatically");
    expect(html).not.toContain("pull the new Docker image");
    // Reconnecting is one console's setting now (This PS5).
    expect(html).not.toContain("Reconnect automatically after rest mode");
    expect(render({ tauri: true, mobile: false }, "/settings?tab=ps5")).toContain(
      "Reconnect automatically after rest mode",
    );
  });

  it("Android has no engine URL", () => {
    const html = render({ tauri: true, mobile: true });
    expect(html).not.toContain("Engine URL");
    expect(html).toContain("Keep screen on");
  });

  it("the browser build hides the native-only settings and explains updates", () => {
    const html = render({ tauri: false, mobile: false });
    for (const gone of [
      "Engine URL",
      "Keep computer awake",
      "Settings file",
      "resolving…",
      "Show system notifications",
      "Reconnect automatically after rest mode",
      "Check for updates automatically",
    ]) {
      expect(html).not.toContain(gone);
    }
    // This PS5 in the browser: no reconnect loop and no bring-up (they send from this machine).
    const ps5 = render({ tauri: false, mobile: false }, "/settings?tab=ps5");
    expect(ps5).toContain("Keep the PS5 awake");
    expect(ps5).not.toContain("Reconnect automatically after rest mode");
    expect(ps5).not.toContain("Bring up automatically");
    expect(html).toContain("pull the new Docker image");
    // The page can keep the screen on, which keeps the tab driving the queue awake.
    expect(html).toContain("Keep screen on");
    expect(html).toContain("this browser keeps");
  });

  it("drops the removed settings and the power-tick reminder", () => {
    const html = render({ tauri: true, mobile: false });
    for (const gone of [
      "Color blind palette",
      "Screen reader hints",
      "Density",
      "0.4×",
      "PS5 power tick",
      "Open Bug Report",
    ]) {
      expect(html).not.toContain(gone);
    }
  });

  it("groups the console's rest-mode settings together (This PS5)", () => {
    const html = render({ tauri: true, mobile: false }, "/settings?tab=ps5");
    const at = (s: string) => html.indexOf(s);
    const console = at(">Console<");
    const uploads = at(">Uploads<");
    expect(console).toBeGreaterThan(-1);
    for (const s of [
      "Reconnect automatically after rest mode",
      "Keep the PS5 awake",
      "Put the PS5 in rest mode after uploads finish",
    ]) {
      expect(at(s)).toBeGreaterThan(console);
      expect(at(s)).toBeLessThan(uploads);
    }
  });
});
