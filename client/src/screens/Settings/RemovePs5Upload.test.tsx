import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { removePs5UploadFlow, removedMessage, type RemoveDeps } from "./RemovePs5Upload";
import type { HelperUninstallResult } from "../../api/ps5";
import type { StoredCopy } from "../../lib/helperStoredCopies";
import type { Translator } from "../../state/lang";
import type { ConfirmOptions } from "../../components/ConfirmDialog";

const tr = ((_k: string, a?: unknown, b?: string) => {
  const s = typeof a === "string" ? a : (b ?? _k);
  const vars = (typeof a === "object" && a) || {};
  return s.replace(/\{(\w+)\}/g, (_, k: string) => String((vars as Record<string, unknown>)[k] ?? ""));
}) as Translator;

const HOST = "192.0.2.77";
const REMOVED: HelperUninstallResult = {
  state: "removed",
  tile: "removed",
  tile_failed: false,
  icons_shown: 0,
  keep_backups: true,
  exited: true,
};

const PM: StoredCopy = { path: "/data/pldmgr/payloads/ps5upload/ps5upload.elf", place: "payload-manager", atBoot: true };
const AUTO: StoredCopy = { path: "/data/ps5_autoloader/ps5upload.elf", place: "autoloader", atBoot: true };

function deps(over: Partial<RemoveDeps> = {}) {
  const calls: string[] = [];
  const d = {
    confirm: vi.fn(async (_o: ConfirmOptions) => true),
    busy: vi.fn(() => false),
    findCopies: vi.fn(async () => [PM, AUTO]),
    disableAtBoot: vi.fn(async () => {
      calls.push("disableAtBoot");
    }),
    pmDelete: vi.fn(async (_h: string, f: string) => {
      calls.push(`pmDelete ${f}`);
    }),
    fsDelete: vi.fn(async (_h: string, p: string) => {
      calls.push(`fsDelete ${p}`);
    }),
    uninstall: vi.fn(async (_h: string, keep: boolean, check: boolean): Promise<HelperUninstallResult> => {
      calls.push(`uninstall keep=${keep} check=${check}`);
      return check ? { state: "ready" } : REMOVED;
    }),
    onRemoved: vi.fn(),
    ...over,
  };
  return { d, calls };
}

describe("Remove PS5Upload from this PS5", () => {
  it("checks first, removes the boot copies while the helper runs, then the helper", async () => {
    const { d, calls } = deps();
    const out = await removePs5UploadFlow(HOST, true, d, tr);
    expect(out.kind).toBe("removed");
    expect(calls).toEqual([
      "uninstall keep=true check=true",
      "disableAtBoot",
      "pmDelete ps5upload.elf",
      `fsDelete ${AUTO.path}`,
      "uninstall keep=true check=false",
    ]);
    expect(d.onRemoved).toHaveBeenCalledWith(HOST);
  });

  it("names every copy it will delete, and the backups when they go", async () => {
    const confirm = vi.fn(async (_o: ConfirmOptions) => false);
    const { d, calls } = deps({ confirm });
    const out = await removePs5UploadFlow(HOST, false, d, tr);
    expect(out.kind).toBe("cancelled");
    const o = confirm.mock.calls[0][0];
    expect(o.destructive).toBe(true);
    expect(o.details).toEqual([PM.path, AUTO.path]);
    expect(o.message).toContain("save backups made on this PS5 are deleted too");
    // Cancelled: only the check ran.
    expect(calls).toEqual(["uninstall keep=false check=true"]);
    expect(d.onRemoved).not.toHaveBeenCalled();
  });

  it("deletes nothing when the helper would refuse (something mounted)", async () => {
    const uninstall = vi.fn(
      async (): Promise<HelperUninstallResult> => ({
        state: "refused",
        reason: "in_use",
        error: "",
        paths: ["/data/ps5upload/mounts/game"],
      }),
    );
    const { d, calls } = deps({ uninstall });
    const out = await removePs5UploadFlow(HOST, true, d, tr);
    expect(out).toEqual({
      kind: "refused",
      message: expect.stringContaining("/data/ps5upload/mounts/game"),
    });
    expect(d.confirm).not.toHaveBeenCalled();
    expect(calls).toEqual([]);
    expect(d.fsDelete).not.toHaveBeenCalled();
  });

  it("stops before the helper goes when a boot copy cannot be deleted", async () => {
    const fsDelete = vi.fn(async () => {
      throw new Error("permission denied");
    });
    const { d } = deps({ fsDelete });
    const out = await removePs5UploadFlow(HOST, true, d, tr);
    expect(out.kind).toBe("refused");
    if (out.kind === "refused") expect(out.message).toContain(AUTO.path);
    expect(d.uninstall).toHaveBeenCalledTimes(1); // the check only
    expect(d.onRemoved).not.toHaveBeenCalled();
  });

  it("falls back to deleting a Payload Manager copy directly when Payload Manager refuses", async () => {
    const pmDelete = vi.fn(async () => {
      throw new Error("no Payload Manager");
    });
    const { d, calls } = deps({ pmDelete, findCopies: vi.fn(async () => [PM]) });
    const out = await removePs5UploadFlow(HOST, true, d, tr);
    expect(out.kind).toBe("removed");
    expect(calls).toContain(`fsDelete ${PM.path}`);
  });

  it("refuses while this app is using the console, before asking anything", async () => {
    const { d } = deps({ busy: vi.fn(() => true) });
    const out = await removePs5UploadFlow(HOST, true, d, tr);
    expect(out.kind).toBe("refused");
    expect(d.uninstall).not.toHaveBeenCalled();
  });

  it("says when the helper is too old or not running", async () => {
    for (const reason of ["old_helper", "not_running"]) {
      const { d } = deps({
        uninstall: vi.fn(async (): Promise<HelperUninstallResult> => ({ state: "refused", reason, error: "", paths: [] })),
      });
      const out = await removePs5UploadFlow(HOST, true, d, tr);
      expect(out.kind).toBe("refused");
      if (out.kind === "refused") expect(out.message).toMatch(reason === "old_helper" ? /too old/ : /isn't running/);
    }
  });

  it("words the result: a hidden tile and icons shown again", () => {
    const text = removedMessage(
      { kind: "removed", copies: 1, result: { ...REMOVED, tile: "hidden", icons_shown: 3 } as never },
      tr,
    );
    expect(text).toContain("is removed from this PS5");
    expect(text).toContain("icon is hidden instead");
    expect(text).toContain("3 hidden icons are shown again");
  });
});
