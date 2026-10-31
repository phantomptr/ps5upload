import { isValidElement, type ReactElement, type ReactNode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import de from "../locales/de.json";
import { _setStringsLoaderForTests, chooseLanguage, getLanguage, storedLanguage, stringsReady, t } from "../strings";
import { ConfirmDialog } from "../ui/ConfirmDialog";
import { canTurnOff, lanToggleStep, SettingsView, startSection, TurnedOff, tileNote, waitTileApplied, type SettingsViewProps, type WebSettings } from "./Settings";

function elements(node: ReactNode): ReactElement<Record<string, unknown>>[] {
  const out: ReactElement<Record<string, unknown>>[] = [];
  const walk = (n: ReactNode) => {
    if (Array.isArray(n)) return n.forEach(walk);
    if (!isValidElement(n)) return;
    const el = n as ReactElement<Record<string, unknown>>;
    out.push(el);
    walk(el.props.children as ReactNode);
  };
  walk(node);
  return out;
}

const base = (over: Partial<SettingsViewProps> = {}): SettingsViewProps => ({
  settings: { lan: false, tile: true, version: "7.1.0" },
  loadFailed: false,
  language: "",
  askLan: false,
  busy: false,
  error: null,
  onToggleLan: vi.fn(),
  onConfirmLan: vi.fn(),
  onDismissLan: vi.fn(),
  onToggleTile: vi.fn(),
  onLanguage: vi.fn(),
  ...over,
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  _setStringsLoaderForTests(null);
  chooseLanguage("");
});

describe("settings", () => {
  it("lan_toggle_requires_confirm", () => {
    expect(lanToggleStep(false)).toBe("confirm");
    expect(lanToggleStep(true)).toBe("apply");

    // Asking shows the warning in a confirm dialog; confirming is a separate action.
    const props = base({ askLan: true });
    const dialog = elements(SettingsView(props)).find((e) => e.type === ConfirmDialog);
    expect(dialog).toBeTruthy();
    expect(dialog?.props.body).toBe(t("cw_settings_lan_warning"));
    expect(t("cw_settings_lan_warning")).toBe("Anyone on your network can then use this page and manage your files.");
    expect(props.onConfirmLan).not.toHaveBeenCalled();
    (dialog?.props.onConfirm as () => void)();
    expect(props.onConfirmLan).toHaveBeenCalledTimes(1);

    // No dialog until asked; the toggle only reports the press.
    const idle = base();
    expect(elements(SettingsView(idle)).some((e) => e.type === ConfirmDialog)).toBe(false);
    const lan = elements(SettingsView(idle)).find((e) => e.props.action === "lan");
    (lan?.props.onClick as () => void)();
    expect(idle.onToggleLan).toHaveBeenCalledTimes(1);
  });

  it("turn_off_minion_confirms_first_then_says_how_to_turn_it_back_on", () => {
    // Older helpers send no `enabled`: no action, nothing to turn off.
    expect(canTurnOff({ lan: false, tile: true, version: "7.1.0" })).toBe(false);
    const old = base({ onTurnOff: vi.fn() });
    expect(elements(SettingsView(old)).some((e) => e.props.setting === "minion-off")).toBe(false);

    const onTurnOff = vi.fn();
    const props = base({ settings: { lan: false, tile: true, enabled: true, version: "7.1.19" }, onTurnOff });
    const row = elements(SettingsView(props)).find((e) => e.props.setting === "minion-off");
    expect(row?.props.label).toBe("Turn off PS5Upload Minion");
    (row?.props.onClick as () => void)();
    expect(onTurnOff).toHaveBeenCalledTimes(1);

    // Asking opens a confirm dialog; only confirming turns it off.
    const onConfirmOff = vi.fn();
    const asking = base({
      settings: { lan: false, tile: true, enabled: true, version: "7.1.19" },
      onTurnOff,
      askOff: true,
      onConfirmOff,
    });
    const dialog = elements(SettingsView(asking)).find(
      (e) => e.type === ConfirmDialog && e.props.title === t("cw_settings_minion_off_confirm_title"),
    );
    expect(dialog?.props.danger).toBe(true);
    expect(String(dialog?.props.body)).toContain("Settings → PS5Upload Minion");
    expect(onConfirmOff).not.toHaveBeenCalled();
    (dialog?.props.onConfirm as () => void)();
    expect(onConfirmOff).toHaveBeenCalledTimes(1);

    // Once off, the page says so and how to turn it back on, instead of the settings.
    const off = elements(SettingsView(base({ turnedOff: true })));
    expect(off.some((e) => e.type === TurnedOff)).toBe(true);
    const msg = elements(TurnedOff());
    expect(msg.some((e) => e.props.children === t("cw_minion_off_title"))).toBe(true);
    expect(t("cw_settings_minion_off_confirm_body")).toContain("open the PS5Upload app on your PC");
  });

  it("tile_switch_waits_for_the_real_result", async () => {
    const at = (over: Partial<WebSettings>): WebSettings => ({ lan: false, tile: false, version: "7.1.0", ...over });
    const reads = [at({ tile_busy: true }), at({ tile_busy: false, tile_failed: true })];
    const read = vi.fn(async () => reads.shift()!);
    const sleep = vi.fn(async () => undefined);
    // The reply to the switch says "still working": re-read until it is done; the failure comes back.
    const done = await waitTileApplied(at({ tile_busy: true }), read, sleep);
    expect(read).toHaveBeenCalledTimes(2);
    expect(done?.tile_failed).toBe(true);
    // Already applied: no re-read.
    const read2 = vi.fn();
    expect(await waitTileApplied(at({ tile_busy: false }), read2, sleep)).toMatchObject({ tile_busy: false });
    expect(read2).not.toHaveBeenCalled();
    // A helper that never finishes: gives up after the limit, failed reads are retried.
    const stuck = vi.fn<() => Promise<WebSettings>>().mockRejectedValueOnce(new Error("x")).mockResolvedValue(at({ tile_busy: true }));
    const last = await waitTileApplied(at({ tile_busy: true }), stuck, sleep, 5000, 1000);
    expect(stuck).toHaveBeenCalledTimes(5);
    expect(last?.tile_busy).toBe(true);
  });

  it("tile_note_says_when_removal_could_only_hide_the_icon", () => {
    const at = (over: Partial<WebSettings>): WebSettings => ({ lan: false, tile: false, version: "7.1.18", ...over });
    // #480: the PS5 kept its record, the helper hid the icon instead: the switch says so.
    expect(tileNote(at({ tile_result: "hidden" }))).toBe(t("cw_settings_tile_hidden"));
    const sw = elements(SettingsView(base({ settings: at({ tile_result: "hidden" }) }))).find((e) => e.props.action === "tile");
    expect(sw?.props.note).toBe(t("cw_settings_tile_hidden"));
    // Removed for real, still working, failed, turned back on, or an older helper: no note.
    expect(tileNote(at({ tile_result: "removed" }))).toBeUndefined();
    expect(tileNote(at({ tile_result: "hidden", tile_busy: true }))).toBeUndefined();
    expect(tileNote(at({ tile_result: "hidden", tile_failed: true }))).toBeUndefined();
    expect(tileNote(at({ tile: true, tile_result: "hidden" }))).toBeUndefined();
    expect(tileNote(at({}))).toBeUndefined();
  });

  it("full screen switch shows only where the browser supports it", () => {
    const onToggleFullscreen = vi.fn();
    const on = elements(SettingsView(base({ fullscreen: true, onToggleFullscreen }))).find((e) => e.props.action === "fullscreen");
    expect(on?.props.on).toBe(true);
    (on?.props.onClick as () => void)();
    expect(onToggleFullscreen).toHaveBeenCalledTimes(1);
    const hidden = elements(SettingsView(base({ fullscreen: null }))).find((e) => e.props.action === "fullscreen");
    expect(hidden).toBeUndefined();
  });

  it("language_choice_persists_and_survives_storage_errors", async () => {
    _setStringsLoaderForTests((code) => (code === "de" ? Promise.resolve(de) : Promise.reject(new Error("offline"))));
    vi.spyOn(console, "warn").mockImplementation(() => {});
    const store = new Map<string, string>();
    vi.stubGlobal("localStorage", {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => void store.set(k, v),
      removeItem: (k: string) => void store.delete(k),
    });
    chooseLanguage("de");
    expect(getLanguage()).toBe("de");
    expect(storedLanguage()).toBe("de");
    await stringsReady();
    expect(t("cw_nav_settings")).toBe(de.cw_nav_settings);
    expect(t("cw_nav_settings")).not.toBe("Settings");
    chooseLanguage("");
    expect(storedLanguage()).toBe("");

    // Blocked storage: the choice still applies, nothing throws.
    const boom = () => {
      throw new Error("blocked");
    };
    vi.stubGlobal("localStorage", { getItem: boom, setItem: boom, removeItem: boom });
    expect(() => chooseLanguage("fr")).not.toThrow();
    expect(getLanguage()).toBe("fr");
    expect(storedLanguage()).toBe("");
  });

  it("theme_row_changes_the_theme_and_contrast_switch_toggles", () => {
    const onTheme = vi.fn();
    const onToggleContrast = vi.fn();
    const els = elements(SettingsView(base({ theme: "dark", contrast: false, onTheme, onToggleContrast })));
    const choices = els.filter((e) => e.props["data-theme-choice"] !== undefined);
    expect(choices.map((e) => e.props["data-theme-choice"])).toEqual(["dark", "light", "oled", "peach"]);
    expect(choices.map((e) => e.props["aria-pressed"])).toEqual([true, false, false, false]);
    (choices[3].props.onClick as () => void)();
    expect(onTheme).toHaveBeenCalledWith("peach");
    const sw = els.find((e) => e.props.action === "contrast");
    expect(sw?.props.on).toBe(false);
    (sw?.props.onClick as () => void)();
    expect(onToggleContrast).toHaveBeenCalled();
  });

  it("start_page_choice_shows_with_a_helper_that_saves_it_and_opens_cheats_only_when_saved", () => {
    // An older helper sends no start_cheats: no row, and the page opens on Home.
    const old = elements(SettingsView(base({ onStartPage: vi.fn() })));
    expect(old.some((e) => e.props["data-start-choice"] !== undefined)).toBe(false);
    expect(startSection({}, true)).toBeNull();

    const onStartPage = vi.fn();
    const settings: WebSettings = { lan: false, tile: true, start_cheats: false, version: "10.0.3" };
    const els = elements(SettingsView(base({ settings, onStartPage })));
    const choices = els.filter((e) => e.props["data-start-choice"] !== undefined);
    expect(choices.map((e) => e.props["data-start-choice"])).toEqual(["home", "cheats"]);
    expect(choices.map((e) => e.props["aria-pressed"])).toEqual([true, false]);
    (choices[1].props.onClick as () => void)();
    expect(onStartPage).toHaveBeenCalledWith(true);

    expect(startSection({ start_cheats: true }, true)).toBe("cheats");
    expect(startSection({ start_cheats: true }, false)).toBeNull();
    expect(startSection({ start_cheats: false }, true)).toBeNull();
    expect(startSection(null, true)).toBeNull();
  });
});
