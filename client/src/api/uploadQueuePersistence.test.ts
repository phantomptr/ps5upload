/**
 * The upload queue's browser persistence.
 *
 * In a self-hosted browser session every queue mutation used to call the
 * Tauri-only `upload_queue_save`, which threw BrowserUnsupportedError.
 * The Upload screen caught it and rendered "Save failed — free disk space
 * or fix permissions" over an install that had actually succeeded, which
 * is how it showed up in the #295 hardware run.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => false }));
vi.mock("../lib/invokeLogged", () => ({
  invoke: vi.fn(async () => {
    throw new Error("the browser must never reach the Tauri command");
  }),
}));

import { uploadQueueLoad, uploadQueueSave } from "./ps5";
import { invoke } from "../lib/invokeLogged";

/** Minimal in-memory localStorage — vitest runs these files in node, so
 *  there is no DOM. Mirrors the stub in state/uploadQueue.test.ts. */
function stubStorage() {
  const map = new Map<string, string>();
  const store = {
    getItem: (k: string) => map.get(k) ?? null,
    setItem: (k: string, v: string) => void map.set(k, v),
    removeItem: (k: string) => void map.delete(k),
    clear: () => map.clear(),
  };
  vi.stubGlobal("localStorage", store);
  return store;
}

describe("upload queue persistence in the browser", () => {
  let store: ReturnType<typeof stubStorage>;

  beforeEach(() => {
    vi.unstubAllGlobals();
    store = stubStorage();
    vi.clearAllMocks();
  });

  it("round-trips the queue document without touching Tauri", async () => {
    await uploadQueueSave({ items: [{ id: "a" }] });
    expect(invoke).not.toHaveBeenCalled();
    await expect(uploadQueueLoad()).resolves.toEqual({ items: [{ id: "a" }] });
  });

  it("reads an empty document on first use", async () => {
    await expect(uploadQueueLoad()).resolves.toEqual({});
  });

  it("treats unreadable storage as an empty queue, not an error", async () => {
    // Private windows and blocked site-data throw on access. The queue
    // being unavailable must not take the Upload screen down with it.
    store.getItem = () => {
      throw new Error("SecurityError");
    };
    await expect(uploadQueueLoad()).resolves.toEqual({});
  });

  it("reports a failed write so a full quota is not silent", async () => {
    store.setItem = () => {
      throw new Error("QuotaExceededError");
    };
    await expect(uploadQueueSave({ items: [] })).rejects.toThrow(
      /couldn't save the upload queue/,
    );
  });
});
