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

const { default: SettingsScreen } = await import("./index");

function render(p: { tauri: boolean; mobile: boolean }): string {
  platform.tauri = p.tauri;
  platform.mobile = p.mobile;
  return renderToStaticMarkup(
    <MemoryRouter>
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
    expect(html).toContain("Reconnect automatically after rest mode");
    expect(html).toContain("Check for updates automatically");
    expect(html).not.toContain("pull the new Docker image");
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

  it("groups the console's rest-mode settings together", () => {
    const html = render({ tauri: true, mobile: false });
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
