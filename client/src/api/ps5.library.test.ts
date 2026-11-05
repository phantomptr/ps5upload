import { beforeEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { scanLibrary, scanLibraryChecked } from "./ps5";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => true }));
const call = vi.mocked(invoke);
beforeEach(() => { call.mockReset(); });

it("finds compressed images, case insensitively, alongside raw images", async () => {
  call.mockImplementation(async (cmd, args) => {
    if (cmd === "ps5_volumes") return { volumes: [{ path: "/data", is_placeholder: false }] };
    if (cmd === "ps5_list_dir" && (args as { path: string }).path === "/data")
      return { entries: ["a.exfat", "b.FFPFSC", "c.ffpkg", "d.ffpfs", "notes.txt"].map((name) => ({ name, kind: "file", size: 123 })) };
    throw new Error(`unexpected ${cmd}`);
  });
  const entries = await scanLibrary("192.0.2.8");
  expect(entries.map((e) => e.imageFormat)).toEqual(["exfat", "ffpfsc", "ffpkg", "ffpfs"]);
});

it("keeps images beyond the first directory page", async () => {
  call.mockImplementation(async (cmd, args) => {
    if (cmd === "ps5_volumes") return { volumes: [{ path: "/data", is_placeholder: false }] };
    if (cmd === "ps5_list_dir") {
      const { offset } = args as { offset: number };
      return offset === 0
        ? { entries: [{ name: "notes.txt", kind: "file" }], truncated: true }
        : { entries: [{ name: "last.ffpfsc", kind: "file", size: 20 }], truncated: false };
    }
    throw new Error(`unexpected ${cmd}`);
  });
  expect((await scanLibrary("192.0.2.8")).map((e) => e.name)).toEqual(["last.ffpfsc"]);
  expect(call).toHaveBeenCalledWith("ps5_list_dir", expect.objectContaining({ offset: 1 }));
});

it("keeps what it read and says incomplete when a folder cannot be read, after one more try", async () => {
  let tries = 0;
  call.mockImplementation(async (cmd, args) => {
    if (cmd === "ps5_volumes") return { volumes: [{ path: "/data", is_placeholder: false }] };
    if (cmd === "ps5_list_dir") {
      const { offset } = args as { offset: number };
      if (offset === 0) return { entries: [{ name: "first.ffpfsc", kind: "file", size: 20 }], truncated: true };
      tries++;
      throw new Error("mgmt read timed out");
    }
    throw new Error(`unexpected ${cmd}`);
  });
  const r = await scanLibraryChecked("192.0.2.8");
  expect(r.entries.map((e) => e.name)).toEqual(["first.ffpfsc"]);
  expect(r.incomplete).toBe(true);
  expect(tries).toBe(2);
});

it("recovers when the second try reads the folder", async () => {
  let failed = false;
  call.mockImplementation(async (cmd) => {
    if (cmd === "ps5_volumes") return { volumes: [{ path: "/data", is_placeholder: false }] };
    if (cmd === "ps5_list_dir") {
      if (!failed) {
        failed = true;
        throw new Error("busy");
      }
      return { entries: [{ name: "a.ffpfsc", kind: "file", size: 20 }] };
    }
    throw new Error(`unexpected ${cmd}`);
  });
  const r = await scanLibraryChecked("192.0.2.8");
  expect(r.entries.map((e) => e.name)).toEqual(["a.ffpfsc"]);
  expect(r.incomplete).toBe(false);
});

it("does not count a folder that is missing or denied as a failure", async () => {
  for (const msg of [
    "payload rejected FS_LIST_DIR(/data): fs_list_dir_opendir_errno_2",
    "payload rejected FS_LIST_DIR(/data): fs_list_dir_opendir_errno_13",
  ]) {
    call.mockReset();
    call.mockImplementation(async (cmd) => {
      if (cmd === "ps5_volumes") return { volumes: [{ path: "/data", is_placeholder: false }] };
      throw new Error(msg);
    });
    const r = await scanLibraryChecked("192.0.2.8");
    expect(r).toEqual({ entries: [], incomplete: false });
    expect(call.mock.calls.filter(([c]) => c === "ps5_list_dir")).toHaveLength(1);
  }
});
