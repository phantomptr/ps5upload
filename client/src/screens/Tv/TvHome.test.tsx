import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter } from "react-router";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { leaveAfterAsking, tvCardState, tvFocusTarget, tvPrimary } from "./tvCard";
import { escapeGoesBack } from "../../layout/TvModeFrame";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../../state/lang", async (orig) => ({
  ...(await orig<typeof import("../../state/lang")>()),
  useTr: () =>
    (key: string, vars?: unknown, fallback?: string) =>
      typeof vars === "string" ? vars : (fallback ?? key),
}));
// The screen reads the stores' live state, so what each test sets is what it shows.
vi.mock("zustand", async (orig) => {
  const { liveZustand } = await import("../../test-utils/liveZustand");
  return liveZustand(await orig<typeof import("zustand")>());
});

const { default: TvHome } = await import("./index");
const { useRosterStore } = await import("../../state/roster");
const { useConnectionStore, EMPTY_HOST_RUNTIME } = await import("../../state/connection");
const { useRemotePlayStore } = await import("../../state/remotePlay");
type Profiles = ReturnType<typeof useRosterStore.getState>["profiles"];

const LIVING: Profiles[number] = {
  id: "lr",
  name: "Living room",
  host: "192.168.1.50",
  wake_regist_key: "00112233",
  wake_rp_key: "0f".repeat(16),
};
const BEDROOM: Profiles[number] = { id: "bd", name: "Bedroom", host: "192.168.1.51" };

function setUp(profiles: Profiles, active: string | null, power: "awake" | "rest" | "unreachable" | "unknown" = "unknown") {
  useRosterStore.setState({ profiles, active_id: active });
  useConnectionStore.setState({
    runtimeByHost: Object.fromEntries(profiles.map((p) => [p.host, { ...EMPTY_HOST_RUNTIME, power }])),
  });
  useRemotePlayStore.setState({ stream: null, phase: null, target: null });
}

const render = () =>
  renderToStaticMarkup(
    <MemoryRouter initialEntries={["/tv"]}>
      <TvHome />
    </MemoryRouter>,
  );

const has = (html: string, id: string) => html.includes(`data-testid="${id}"`);

describe("TV mode's home card (#976)", () => {
  beforeEach(() => setUp([LIVING], "lr"));

  it("with no console: Add your PS5, and joining a friend's game", () => {
    setUp([], null);
    const html = render();
    expect(html).toContain('data-state="none"');
    expect(html).toContain("No PS5 yet");
    expect(has(html, "tv-add")).toBe(true);
    expect(has(html, "tv-join-go")).toBe(true);
    expect(has(html, "tv-play")).toBe(false);
    expect(has(html, "tv-settings-open")).toBe(false);
  });

  it("a paired console that is on: its name, On, a big Play and the settings", () => {
    setUp([LIVING], "lr", "awake");
    const html = render();
    expect(html).toContain('data-state="on"');
    expect(html).toContain("Living room");
    expect(html).toMatch(/data-testid="tv-console-state"[^>]*>.*On</);
    expect(has(html, "tv-play")).toBe(true);
    expect(html).toMatch(/data-tv-primary=""[^>]*data-testid="tv-play"|data-testid="tv-play"[^>]*data-tv-primary=""/);
    expect(has(html, "tv-settings-open")).toBe(true);
    expect(has(html, "tv-full-app")).toBe(true);
    // One console: nothing to switch to.
    expect(has(html, "tv-switch")).toBe(false);
  });

  it("in rest mode Play wakes it; off or unknown, Play is still there", () => {
    setUp([LIVING], "lr", "rest");
    let html = render();
    expect(html).toContain('data-state="rest"');
    expect(html).toContain("In rest mode. Play wakes it.");
    expect(has(html, "tv-play")).toBe(true);
    setUp([LIVING], "lr", "unreachable");
    html = render();
    expect(html).toContain('data-state="unreachable"');
    expect(html).toContain("Off or not reachable. Play tries to wake it.");
    expect(has(html, "tv-play")).toBe(true);
    setUp([LIVING], "lr", "unknown");
    html = render();
    expect(html).toContain('data-state="checking"');
    expect(has(html, "tv-play")).toBe(true);
  });

  it("not paired: a short way into Remote Play's setup instead of Play", () => {
    setUp([{ ...LIVING, wake_regist_key: undefined, wake_rp_key: undefined }], "lr", "awake");
    const html = render();
    expect(html).toContain('data-state="setup"');
    expect(has(html, "tv-setup")).toBe(true);
    expect(has(html, "tv-play")).toBe(false);
  });

  it("while a stream plays: Resume and Stop, with what is playing", () => {
    setUp([LIVING], "lr", "awake");
    useRemotePlayStore.setState({
      stream: {} as never,
      phase: "playing",
      target: { host: LIVING.host, name: "Living room", registKey: "a", rpKey: "b" },
    });
    const html = render();
    expect(html).toContain('data-state="playing"');
    expect(html).toContain("Playing Living room");
    expect(has(html, "tv-resume")).toBe(true);
    expect(has(html, "tv-stop")).toBe(true);
    expect(has(html, "tv-play")).toBe(false);
  });

  it("remembers the last console used, and offers a switch with several", () => {
    setUp([LIVING, BEDROOM], "bd", "awake");
    const html = render();
    expect(html).toContain("Bedroom");
    expect(html).not.toContain("Living room");
    expect(has(html, "tv-switch")).toBe(true);
    // Bedroom was never paired: its own card says so.
    expect(html).toContain('data-state="setup"');
  });

  it("never names another tool", () => {
    setUp([LIVING], "lr", "awake");
    expect(render()).not.toMatch(/chiaki|PS Remote Play app|Moonlight/i);
  });
});

describe("TV mode's card states and focus", () => {
  it("maps what the app knows to one state", () => {
    const base = { hasConsole: true, paired: true, streaming: false, power: "awake" as const };
    expect(tvCardState({ ...base, hasConsole: false })).toBe("none");
    expect(tvCardState({ ...base, streaming: true, paired: false })).toBe("playing");
    expect(tvCardState({ ...base, paired: false })).toBe("setup");
    expect(tvCardState(base)).toBe("on");
    expect(tvCardState({ ...base, power: "rest" })).toBe("rest");
    expect(tvCardState({ ...base, power: "unreachable" })).toBe("unreachable");
    expect(tvCardState({ ...base, power: "unknown" })).toBe("checking");
  });

  it("the remote starts on Play (or Resume, or the setup), and on Join with no console", () => {
    expect(tvFocusTarget(tvPrimary("on"))).toBe("tv-play");
    expect(tvFocusTarget(tvPrimary("rest"))).toBe("tv-play");
    expect(tvFocusTarget(tvPrimary("checking"))).toBe("tv-play");
    expect(tvFocusTarget(tvPrimary("playing"))).toBe("tv-resume");
    expect(tvFocusTarget(tvPrimary("setup"))).toBe("tv-setup");
    expect(tvFocusTarget(tvPrimary("none"))).toBe("tv-join-go");
  });
});

describe("Back in TV mode", () => {
  it("on the home asks, and leaves the app only on a yes", async () => {
    const leave = vi.fn();
    expect(await leaveAfterAsking(async () => false, leave)).toBe(false);
    expect(leave).not.toHaveBeenCalled();
    expect(await leaveAfterAsking(async () => true, leave)).toBe(true);
    expect(leave).toHaveBeenCalledTimes(1);
  });

  it("a keyboard's Esc is Back, except in a dialog (it closes) and on the game's picture", () => {
    const key = (patch: Partial<KeyboardEvent> = {}) =>
      ({
        key: "Escape",
        defaultPrevented: false,
        altKey: false,
        ctrlKey: false,
        metaKey: false,
        shiftKey: false,
        target: { closest: () => null },
        ...patch,
      }) as unknown as KeyboardEvent;
    expect(escapeGoesBack(key(), false)).toBe(true);
    expect(escapeGoesBack(key(), true)).toBe(false);
    expect(escapeGoesBack(key({ defaultPrevented: true }), false)).toBe(false);
    expect(escapeGoesBack(key({ key: "Enter" }), false)).toBe(false);
    expect(escapeGoesBack(key({ shiftKey: true }), false)).toBe(false);
    const onPicture = key({ target: { closest: (s: string) => (s === "[data-own-arrows]" ? {} : null) } as never });
    expect(escapeGoesBack(onPicture, false)).toBe(false);
  });
});
