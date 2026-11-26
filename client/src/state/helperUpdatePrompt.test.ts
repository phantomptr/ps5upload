import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

// The app's storage, kept across "restarts" of the module state (no DOM here).
const disk = vi.hoisted(() => ({ items: new Map<string, string>() }));
vi.mock("../lib/safeStorage", async (orig) => ({
  ...(await orig<typeof import("../lib/safeStorage")>()),
  safeGetItem: (k: string) => disk.items.get(k) ?? null,
  safeSetItem: (k: string, v: string) => void disk.items.set(k, v),
}));
vi.mock("../lib/osNotify", () => ({
  sendOsNotification: vi.fn(),
  appIsForeground: () => true,
  ensureOsNotificationPermission: vi.fn(async () => false),
}));

import { EMPTY_HOST_RUNTIME, useConnectionStore, type HostRuntime } from "./connection";
import { useHelperSendStore } from "./helperSend";
import {
  dismissHelperPrompt,
  evaluateHelperPrompts,
  helperUpdateHold,
  holdText,
  requestHelperUpdate,
  resetHelperUpdatePrompt,
  runHelperUpdate,
  takeHelperUpdateRequest,
  useHelperUpdatePromptStore,
} from "./helperUpdatePrompt";
import type { Translator } from "./lang";
import { useNotificationsStore } from "./notifications";
import { useRemotePlayStore } from "./remotePlay";
import { useRosterStore } from "./roster";
import { useTaskStore } from "./tasks";
import { useToastStore } from "./toasts";
import { useUploadQueueStore } from "./uploadQueue";
import { helperAutoUpdateGate } from "../lib/helperAutoUpdate";
import { beginLaunch, resetLaunchInFlight } from "../lib/launchInFlight";

const HOST = "192.168.1.20";
const OTHER = "192.168.1.21";
const tr: Translator = ((_k: string, a?: unknown, b?: string) => (typeof a === "string" ? a : (b ?? ""))) as Translator;

function consoles(byHost: Record<string, Partial<HostRuntime>>) {
  useConnectionStore.setState({
    host: HOST,
    runtimeByHost: Object.fromEntries(
      Object.entries(byHost).map(([h, r]) => [h, { ...EMPTY_HOST_RUNTIME, power: "awake", ...r } as HostRuntime]),
    ),
  });
}
const up = (payloadVersion: string): Partial<HostRuntime> => ({ payloadStatus: "up", payloadVersion });
const toasts = () => useToastStore.getState().toasts;
const inbox = () => useNotificationsStore.getState().entries;
const notice = (h = HOST) => useHelperUpdatePromptStore.getState().byHost[h];
/** The app closed and opened again: what it kept is only what it stored. */
const restart = () => {
  resetHelperUpdatePrompt();
  useToastStore.setState({ toasts: [] });
};

beforeEach(() => {
  disk.items.clear();
  helperAutoUpdateGate.available = false;
  resetHelperUpdatePrompt();
  resetLaunchInFlight();
  useToastStore.getState().dismissAll();
  useNotificationsStore.setState({ entries: [] });
  useHelperSendStore.setState({ byHost: {} });
  useTaskStore.setState({ tasks: [] });
  useRemotePlayStore.setState({ stream: null, target: null });
  useUploadQueueStore.setState({ runningHosts: {}, items: [], running: false });
  useRosterStore.setState({ profiles: [], active_id: null });
  consoles({ [HOST]: up("12.6.30") });
});

describe("the helper is out of date: what the user is told (#941)", () => {
  it("one toast with an Update helper button, one bell entry and the notice on Home", () => {
    evaluateHelperPrompts("12.6.31");
    expect(toasts()).toHaveLength(1);
    expect(toasts()[0].message).toBe("The PS5 helper is out of date");
    expect(toasts()[0].action?.label).toBe("Update helper");
    expect(inbox()).toHaveLength(1);
    expect(inbox()[0].title).toBe("The PS5 helper is out of date");
    expect(inbox()[0].body).toBe(
      "This PS5 runs helper v12.6.30. This app is v12.6.31. Update the helper so new fixes and features work.",
    );
    expect(inbox()[0].link).toBe("/connection");
    expect(notice()).toEqual({ host: HOST, from: "12.6.30", to: "12.6.31" });
    expect(useHelperUpdatePromptStore.getState().outdated[HOST]).toBe(true);
  });

  it("is said once: every later look, and the next start of the app, adds nothing", () => {
    evaluateHelperPrompts("12.6.31");
    evaluateHelperPrompts("12.6.31");
    restart();
    evaluateHelperPrompts("12.6.31");
    expect(toasts()).toHaveLength(0);
    expect(inbox()).toHaveLength(1);
    // The notice on Home is still there: the helper is still older.
    expect(notice()).toBeDefined();
  });

  it("comes back for the next app version", () => {
    evaluateHelperPrompts("12.6.31");
    dismissHelperPrompt(HOST);
    restart();
    evaluateHelperPrompts("12.6.32");
    expect(toasts()).toHaveLength(1);
    expect(inbox()).toHaveLength(2);
    expect(notice()?.to).toBe("12.6.32");
  });

  it("each console is told for itself, and named when there is more than one", () => {
    useRosterStore.setState({
      profiles: [
        { id: "a", name: "Living room", host: HOST },
        { id: "b", name: "Office", host: OTHER },
      ],
      active_id: "a",
    } as never);
    consoles({ [HOST]: up("12.6.30"), [OTHER]: up("12.6.29") });
    evaluateHelperPrompts("12.6.31");
    expect(toasts().map((t) => t.message).sort()).toEqual([
      "Living room: The PS5 helper is out of date",
      "Office: The PS5 helper is out of date",
    ]);
    expect(notice(OTHER)?.from).toBe("12.6.29");
  });

  it("says nothing about a console that is asleep, unreachable or has a current helper", () => {
    consoles({
      [HOST]: { payloadStatus: "down", payloadVersion: "12.6.30", power: "rest" },
      [OTHER]: { payloadStatus: "down", payloadVersion: "12.6.30", power: "unreachable" },
      "192.168.1.22": up("12.6.31"),
    });
    evaluateHelperPrompts("12.6.31");
    expect(toasts()).toHaveLength(0);
    expect(inbox()).toHaveLength(0);
    expect(useHelperUpdatePromptStore.getState().byHost).toEqual({});
    expect(useHelperUpdatePromptStore.getState().outdated).toEqual({});
  });

  it("the notice leaves when the console goes to rest and returns when it wakes, without a second toast", () => {
    evaluateHelperPrompts("12.6.31");
    consoles({ [HOST]: { payloadStatus: "down", payloadVersion: "12.6.30", power: "rest" } });
    evaluateHelperPrompts("12.6.31");
    expect(notice()).toBeUndefined();
    consoles({ [HOST]: up("12.6.30") });
    evaluateHelperPrompts("12.6.31");
    expect(notice()).toBeDefined();
    expect(toasts()).toHaveLength(1);
  });

  it("a dismissed notice stays away, and the helper's row on Home still reads as older", () => {
    evaluateHelperPrompts("12.6.31");
    dismissHelperPrompt(HOST);
    expect(notice()).toBeUndefined();
    restart();
    evaluateHelperPrompts("12.6.31");
    expect(notice()).toBeUndefined();
    expect(useHelperUpdatePromptStore.getState().outdated[HOST]).toBe(true);
  });

  it("once the helper is updated the notice and its bell entry go", () => {
    evaluateHelperPrompts("12.6.31");
    consoles({ [HOST]: up("12.6.31") });
    evaluateHelperPrompts("12.6.31");
    expect(notice()).toBeUndefined();
    expect(inbox()).toHaveLength(0);
    expect(useHelperUpdatePromptStore.getState().outdated).toEqual({});
  });

  it("the notice keeps showing while its own send replaces the helper, though the old one stops answering", () => {
    evaluateHelperPrompts("12.6.31");
    useHelperSendStore.setState({
      byHost: { [HOST]: { state: "busy", phase: null, msg: "Starting the helper…", startedAtMs: 1 } },
    });
    consoles({ [HOST]: { payloadStatus: "down", payloadVersion: null } });
    evaluateHelperPrompts("12.6.31");
    expect(notice()).toBeDefined();
  });

  it("is left to the automatic update where that exists and is on", () => {
    helperAutoUpdateGate.available = true;
    evaluateHelperPrompts("12.6.31");
    expect(toasts()).toHaveLength(0);
    expect(notice()).toBeUndefined();
  });
});

describe("the Update helper button", () => {
  const guard = vi.fn(async () => true);
  const send = vi.fn(async () => "ok" as const);

  beforeEach(() => {
    guard.mockClear();
    guard.mockResolvedValue(true);
    send.mockClear();
  });

  it("the toast's button asks for the update, and one shell takes it once", () => {
    evaluateHelperPrompts("12.6.31");
    toasts()[0].action?.onClick();
    expect(takeHelperUpdateRequest()).toBe(HOST);
    expect(takeHelperUpdateRequest()).toBeNull();
    requestHelperUpdate(OTHER);
    expect(takeHelperUpdateRequest()).toBe(OTHER);
  });

  it("does what Resend helper does: the replace questions first, then the send, over the running version", async () => {
    useConnectionStore.setState({ payloadVersion: "12.6.30" });
    expect(await runHelperUpdate(HOST, tr, guard, send)).toBe("ok");
    expect(guard).toHaveBeenCalledWith(HOST);
    expect(send).toHaveBeenCalledWith(HOST, tr, "12.6.30");
  });

  it("sends nothing when the person says no to a question", async () => {
    guard.mockResolvedValue(false);
    expect(await runHelperUpdate(HOST, tr, guard, send)).toBe("declined");
    expect(send).not.toHaveBeenCalled();
  });

  it("does not send over this app's install or transfer, and says why", async () => {
    useTaskStore.setState({
      tasks: [{ id: "t1", kind: "install", label: "Install Game", consoleId: HOST, status: "running" }],
    } as never);
    expect(helperUpdateHold(HOST)).toBe("transfer");
    expect(await runHelperUpdate(HOST, tr, guard, send)).toBe("held");
    expect(guard).not.toHaveBeenCalled();
    expect(send).not.toHaveBeenCalled();
    expect(useHelperUpdatePromptStore.getState().holds[HOST]).toBe("transfer");
    expect(toasts()[toasts().length - 1]?.message).toBe(
      "A transfer or an install is running on this PS5, and a new helper would stop it. Update the helper when it has finished.",
    );
  });

  it("does not send while the queue runs, a game is starting or Remote Play shows that console", async () => {
    useUploadQueueStore.setState({ runningHosts: { [HOST]: true } } as never);
    expect(helperUpdateHold(HOST)).toBe("transfer");
    useUploadQueueStore.setState({ runningHosts: {} } as never);

    const started = beginLaunch(HOST);
    expect(await runHelperUpdate(HOST, tr, guard, send)).toBe("held");
    expect(holdText(tr, "launch")).toBe("A game is starting on this PS5. Update the helper once it is running.");
    started();

    useRemotePlayStore.setState({ stream: {} as never, target: { host: HOST } as never });
    expect(helperUpdateHold(HOST)).toBe("stream");
    expect(helperUpdateHold(OTHER)).toBeNull();
    expect(send).not.toHaveBeenCalled();
  });

  it("another console's work holds nothing back, and a held press clears once it is sent", async () => {
    useTaskStore.setState({
      tasks: [{ id: "t1", kind: "install", label: "Install Game", consoleId: OTHER, status: "running" }],
    } as never);
    useHelperUpdatePromptStore.setState({ holds: { [HOST]: "transfer" } });
    expect(await runHelperUpdate(HOST, tr, guard, send)).toBe("ok");
    expect(useHelperUpdatePromptStore.getState().holds[HOST]).toBeUndefined();
  });
});
