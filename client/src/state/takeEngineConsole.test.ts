// #907: a console the engine adds (Add a PS5 by its Tailscale address, the Tailscale panel, the
// Tailscale find on Consoles) reaches the app's list through the list mirror, which an app using
// another computer's engine does not run. There it goes on the list directly.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { mockIsTauri } = vi.hoisted(() => ({ mockIsTauri: vi.fn(() => true) }));
vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => mockIsTauri() }));

import { useEngineStore } from "./engine";
import { useRosterStore } from "./roster";
import { rosterMirrorsEngine, takeEngineConsole } from "./rosterShare";
import { goesToEngine } from "../lib/invokeLogged";

const REMOTE = "http://100.70.1.2:19113";
const LOCAL = "http://127.0.0.1:19113";

const pulls: string[] = [];

beforeEach(() => {
  mockIsTauri.mockReturnValue(true);
  useRosterStore.setState({ profiles: [], active_id: null });
  pulls.length = 0;
  vi.stubGlobal("window", { dispatchEvent: (e: Event) => pulls.push(e.type) });
});

afterEach(() => {
  vi.unstubAllGlobals();
  useEngineStore.getState().setEngineUrl(LOCAL);
});

const engineAnswer = { id: "engine-id-1", name: "Living room", host: "100.101.4.7" };

describe("takeEngineConsole", () => {
  it("an app using another computer's engine puts the console on its own list, as the engine named it", () => {
    useEngineStore.getState().setEngineUrl(REMOTE);
    expect(rosterMirrorsEngine()).toBe(false);
    const id = takeEngineConsole(engineAnswer, "100.101.4.7");
    const profiles = useRosterStore.getState().profiles;
    expect(profiles).toHaveLength(1);
    expect(profiles[0]).toMatchObject({ id, name: "Living room", host: "100.101.4.7" });
    expect(pulls).toEqual([]);
  });

  it("uses the typed address and a plain name when the engine gave none", () => {
    useEngineStore.getState().setEngineUrl(REMOTE);
    takeEngineConsole({ id: "x", name: "", host: "" }, "ps5.tail1234.ts.net");
    expect(useRosterStore.getState().profiles[0]).toMatchObject({
      name: "PS5 (ps5.tail1234.ts.net)",
      host: "ps5.tail1234.ts.net",
    });
  });

  it("adds nothing twice: a console already on the list at that address is the one", () => {
    useEngineStore.getState().setEngineUrl(REMOTE);
    const first = takeEngineConsole(engineAnswer, "100.101.4.7");
    // The engine moved its entry to the home address it knows; the typed address still matches.
    const again = takeEngineConsole({ ...engineAnswer, host: "192.168.1.40" }, "100.101.4.7");
    expect(again).toBe(first);
    expect(useRosterStore.getState().profiles).toHaveLength(1);
  });

  it("where the list mirrors (the app's own engine), it only asks the mirror and adds nothing here", () => {
    useEngineStore.getState().setEngineUrl(LOCAL);
    expect(rosterMirrorsEngine()).toBe(true);
    expect(takeEngineConsole(engineAnswer, "100.101.4.7")).toBe("engine-id-1");
    expect(useRosterStore.getState().profiles).toEqual([]);
    expect(pulls).toEqual(["ps5upload:roster-pull"]);
  });

  it("the browser's page mirrors too, whatever the engine's address", () => {
    useEngineStore.getState().setEngineUrl(REMOTE);
    mockIsTauri.mockReturnValue(false);
    expect(rosterMirrorsEngine()).toBe(true);
    expect(takeEngineConsole(engineAnswer, "100.101.4.7")).toBe("engine-id-1");
    expect(useRosterStore.getState().profiles).toEqual([]);
  });
});

describe("the PS5 scan runs where the engine is", () => {
  it("on another computer's engine, the scan goes to it; on the app's own, it stays native", () => {
    useEngineStore.getState().setEngineUrl(REMOTE);
    expect(goesToEngine("discover_ps5")).toBe(true);
    useEngineStore.getState().setEngineUrl(LOCAL);
    expect(goesToEngine("discover_ps5")).toBe(false);
  });

  it("other native commands stay native with a remote engine", () => {
    useEngineStore.getState().setEngineUrl(REMOTE);
    expect(goesToEngine("payload_send")).toBe(false);
    expect(goesToEngine("local_list_dir")).toBe(true);
  });
});
