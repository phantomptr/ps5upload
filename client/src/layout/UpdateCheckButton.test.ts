import { afterEach, describe, expect, it, vi } from "vitest";

import { checkForUpdatesNow, updateWaiting } from "./UpdateCheckButton";
import { useUpdateStore } from "../state/update";
import { useToastStore } from "../state/toasts";
import type { Translator } from "../state/lang";
import type { UpdateCheck } from "../api/ps5";

const tr = ((_k: string, vars: Record<string, string> | undefined, fallback: string) =>
  fallback.replace(/\{(\w+)\}/g, (_, n) => vars?.[n] ?? "")) as unknown as Translator;
const result = (latest: string, available: boolean) =>
  ({ available, current_version: "7.4.2", latest_version: latest, notes: "", pub_date: "" }) as UpdateCheck;

describe("check for updates", () => {
  afterEach(() => {
    useToastStore.setState({ toasts: [] });
    vi.restoreAllMocks();
  });

  it("knows an update is waiting through download and install", () => {
    expect(updateWaiting({ kind: "available", result: result("7.4.3", true) })).toBe("7.4.3");
    expect(updateWaiting({ kind: "downloading", result: result("7.4.3", true) })).toBe("7.4.3");
    expect(updateWaiting({ kind: "up-to-date", result: result("7.4.2", false) })).toBeNull();
    expect(updateWaiting({ kind: "idle" })).toBeNull();
  });

  it("says when you are up to date", async () => {
    useUpdateStore.setState({
      phase: { kind: "idle" },
      checkNow: async () => useUpdateStore.setState({ phase: { kind: "up-to-date", result: result("7.4.2", false) } }),
    });
    await checkForUpdatesNow(tr, () => {});
    expect(useToastStore.getState().toasts.map((t) => t.message)).toEqual([
      "You're on the latest version (v7.4.2)",
    ]);
  });

  it("says when the check failed", async () => {
    useUpdateStore.setState({
      phase: { kind: "idle" },
      checkNow: async () => useUpdateStore.setState({ phase: { kind: "error", message: "offline" } }),
    });
    await checkForUpdatesNow(tr, () => {});
    expect(useToastStore.getState().toasts[0]).toMatchObject({
      tone: "warn",
      message: "Couldn't check for updates: offline",
    });
  });

  it("an update the check finds opens Settings, whose top row offers it (no banner shows it)", async () => {
    const open = vi.fn();
    useUpdateStore.setState({
      phase: { kind: "idle" },
      checkNow: async () => useUpdateStore.setState({ phase: { kind: "available", result: result("7.4.3", true) } }),
    });
    await checkForUpdatesNow(tr, open);
    expect(open).toHaveBeenCalledOnce();
    expect(useToastStore.getState().toasts).toEqual([]);
  });

  it("with an update already found, opens Settings instead of checking again", async () => {
    const checkNow = vi.fn(async () => {});
    const open = vi.fn();
    useUpdateStore.setState({ phase: { kind: "available", result: result("7.4.3", true) }, checkNow });
    await checkForUpdatesNow(tr, open);
    expect(open).toHaveBeenCalledOnce();
    expect(checkNow).not.toHaveBeenCalled();
  });
});
