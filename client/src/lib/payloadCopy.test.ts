import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { pickedCopy, savedCopy } from "./payloadCopy";

describe("which copy a payload send uses", () => {
  const inventory = [
    { path: "/lib/shadowmountplus/1.4/smp.elf", version: "1.4" },
    { path: "/lib/shadowmountplus/1.3/smp.elf", version: "1.3" },
  ];

  it("names the saved copy by its version, else by its file name", () => {
    expect(savedCopy("/lib/shadowmountplus/1.3/smp.elf", inventory)).toEqual({
      path: "/lib/shadowmountplus/1.3/smp.elf",
      label: "1.3",
      picked: false,
    });
    expect(savedCopy("/lib/x/np-fake-signin.elf", inventory)?.label).toBe("np-fake-signin.elf");
    expect(savedCopy("/lib/x/a.elf", [{ path: "/lib/x/a.elf", version: " " }])?.label).toBe("a.elf");
    expect(savedCopy(null, inventory)).toBeNull();
  });

  it("a chosen file is named by its file name; a cancelled picker is nothing", () => {
    expect(pickedCopy("C:\\Users\\me\\smp-1.5.elf")).toEqual({
      path: "C:\\Users\\me\\smp-1.5.elf",
      label: "smp-1.5.elf",
      picked: true,
    });
    expect(pickedCopy(null)).toBeNull();
    expect(pickedCopy("")).toBeNull();
  });
});
