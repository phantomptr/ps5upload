import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ApiError } from "../api";
import { t } from "../strings";
import { displayVersion, playAppErrorText, PlayAppCardView, playAppView, type PlayAppCardViewProps, type PlayAppState } from "./PlayAppCard";

const idle = { state: "idle" as const, done: 0, total: 0 };

const st = (over: Partial<PlayAppState> = {}): PlayAppState => ({
  installed: { version: "13.000.000", path: "/data/homebrew/FAKE99261", registered: true },
  staged: { version: "13.000.000" },
  store_managed: false,
  store_busy: false,
  running: false,
  install: idle,
  ...over,
});

const noop = () => {};
const render = (over: Partial<PlayAppCardViewProps> = {}) =>
  renderToStaticMarkup(
    <PlayAppCardView
      state={st()}
      message={null}
      asking={false}
      pending={false}
      onInstall={noop}
      onConfirmStore={noop}
      onCancelStore={noop}
      onOpen={noop}
      {...over}
    />,
  );

describe("displayVersion", () => {
  it("shows param.json's fixed form as people read it", () => {
    expect(displayVersion("13.000.000")).toBe("13.0.0");
    expect(displayVersion("13.001.020")).toBe("13.1.20");
    expect(displayVersion("1.0")).toBe("1.0");
    expect(displayVersion("beta")).toBe("beta");
  });
});

describe("playAppView", () => {
  it("offers Install when only the staged copy is there", () => {
    expect(playAppView(st({ installed: null }))).toMatchObject({ action: "install", canOpen: false, asks: false });
  });
  it("offers Update only when the versions differ", () => {
    expect(playAppView(st()).action).toBeNull();
    expect(playAppView(st({ staged: { version: "13.001.000" } })).action).toBe("update");
    expect(playAppView(st({ staged: null })).action).toBeNull();
  });
  it("installs again when the same version is not on the home screen", () => {
    const s = st({ installed: { version: "13.000.000", path: "/data/homebrew/FAKE99261", registered: false } });
    expect(playAppView(s)).toMatchObject({ action: "install", canOpen: false });
  });
  it("asks first over a copy another app installed", () => {
    expect(playAppView(st({ staged: { version: "13.001.000" }, store_managed: true })).asks).toBe(true);
    expect(playAppView(st({ store_managed: true })).asks).toBe(false); /* nothing to do */
  });
  it("is busy while an install runs", () => {
    for (const s of ["closing", "copying", "registering"] as const)
      expect(playAppView(st({ install: { state: s, done: 1, total: 2 } })).busy).toBe(true);
    expect(playAppView(st({ install: { state: "failed", done: 0, total: 0, error: "x" } })).busy).toBe(false);
  });
});

describe("PlayAppCardView", () => {
  it("is hidden with nothing installed and nothing to install", () => {
    expect(render({ state: st({ installed: null, staged: null }) })).toBe("");
  });
  it("shows the installed version and Open", () => {
    const html = render();
    expect(html).toContain(t("cw_play_installed", { version: "13.0.0" }));
    expect(html).toContain('data-action="play-app-open"');
    expect(html).not.toContain("play-app-update");
  });
  it("shows Update with both versions, and that the app closes first while it runs", () => {
    const html = render({ state: st({ staged: { version: "13.001.000" }, running: true }) });
    expect(html).toContain(t("cw_play_update_ready", { version: "13.1.0", installed: "13.0.0" }));
    expect(html).toContain('data-action="play-app-update"');
    expect(html).toContain(t("cw_play_closes_first"));
  });
  it("shows progress and no buttons while installing", () => {
    const html = render({ state: st({ staged: { version: "13.001.000" }, install: { state: "copying", done: 50, total: 200 } }) });
    expect(html).toContain(t("cw_play_installing", { pct: 25 }));
    expect(html).not.toContain("data-action=");
  });
  it("holds Install while another app is installing it", () => {
    const html = render({ state: st({ installed: null, store_busy: true }) });
    expect(html).toContain(t("cw_play_store_busy"));
    expect(html).toMatch(/data-action="play-app-install"[^>]*disabled=""|disabled=""[^>]*data-action="play-app-install"/);
  });
  it("asks before replacing another app's copy", () => {
    const html = render({ state: st({ staged: { version: "13.001.000" }, store_managed: true }), asking: true });
    expect(html).toContain(t("cw_play_store_title"));
  });
});

describe("playAppErrorText", () => {
  it("names the cases people can act on", () => {
    expect(playAppErrorText(new ApiError(409, "store_busy"))).toBe(t("cw_play_store_busy"));
    expect(playAppErrorText(new ApiError(409, "other_game_running"))).toBe(t("cw_games_err_other_running"));
    expect(playAppErrorText(new ApiError(0, "offline"))).toBe(t("cw_home_err_offline"));
    expect(playAppErrorText(new ApiError(401, "token"))).toBeNull();
    expect(playAppErrorText(new ApiError(409, "copy_failed"))).toBe(t("cw_play_failed", { code: "copy_failed" }));
  });
});
