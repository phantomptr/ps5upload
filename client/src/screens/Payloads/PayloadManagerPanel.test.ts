import { describe, expect, it } from "vitest";

import { filterSaved, moveEntry, runningOrigin, SAVED_FILTER_MIN, unmatchedBootEntries } from "./PayloadManagerPanel";

const tr = ((_k: string, vars: Record<string, unknown> | undefined, fallback: string) =>
  fallback.replace(/\{(\w+)\}/g, (_, n) => String(vars?.[n] ?? ""))) as never;

const stored = (slot: string, filename: string) => ({
  slot,
  filename,
  path: `/data/pldmgr/payloads/${slot || "x"}/${filename}`,
});

describe("boot-list entries that name no stored payload", () => {
  const payloads = [stored("local/kstuff", "kstuff-1.13-fpkg.elf"), stored("", "kraken-mount.elf")];

  it("lists them with their boot number, pauses not counted", () => {
    // 0.6 slot, 0.5 file name, a pause, a deleted file, a USB path not plugged in.
    const list = ["local/kstuff", "!2000", "kraken-mount.elf", "local/syslang", "/mnt/usb0/AnyPad.elf"];
    expect(unmatchedBootEntries(list, payloads)).toEqual([
      { entry: "local/syslang", n: 3 },
      { entry: "/mnt/usb0/AnyPad.elf", n: 4 },
    ]);
  });

  it("knows a payload by its slot, file name or path", () => {
    const p = payloads[0];
    expect(unmatchedBootEntries(["local/kstuff", p.filename, p.path], payloads)).toEqual([]);
  });

  it("is empty for an empty list", () => {
    expect(unmatchedBootEntries([], payloads)).toEqual([]);
  });
});

describe("a running saved row's origin", () => {
  it("names its boot place only while the boot list is on", () => {
    expect(runningOrigin(tr, false, 2)).toBe("From your Payload Manager list, starts at boot #2");
    expect(runningOrigin(tr, false, null)).toBe("From your Payload Manager list");
  });

  it("says the helper is PS5Upload's own", () => {
    expect(runningOrigin(tr, true, 1)).toBe("PS5Upload's own helper");
  });
});

describe("moving a boot entry", () => {
  it("swaps with the next payload, keeping pauses in place", () => {
    expect(moveEntry(["a", "!1000", "b", "c"], "a", 1)).toEqual(["b", "!1000", "a", "c"]);
    expect(moveEntry(["a", "!1000", "b"], "a", -1)).toEqual(["a", "!1000", "b"]);
  });
});

describe("narrowing a long saved list", () => {
  const saved = (filename: string, extra: Partial<{ name: string; version: string; source: string }> = {}) => ({
    name: "",
    version: "",
    source: "",
    filename,
    path: `/data/pldmgr/payloads/local/${filename.replace(/\.elf$/, "")}/${filename}`,
    ...extra,
  });
  const list = [
    saved("kstuff-1.13-fpkg.elf"),
    saved("ftpsrv.elf", { name: "FTP server", version: "v0.14", source: "ps5-payload-dev" }),
    saved("websrv.elf"),
    { ...saved("AnyPad-PS5.elf"), path: "/mnt/usb0/payloads/AnyPad-PS5.elf" },
  ];

  it("keeps every payload for an empty query", () => {
    expect(filterSaved(list, "")).toEqual(list);
    expect(filterSaved(list, "   ")).toEqual(list);
  });

  it("finds by file name, name, path, version or source, in any case", () => {
    const names = (q: string) => filterSaved(list, q).map((p) => p.filename);
    expect(names("SRV")).toEqual(["ftpsrv.elf", "websrv.elf"]);
    expect(names("ftp server")).toEqual(["ftpsrv.elf"]);
    expect(names("usb0")).toEqual(["AnyPad-PS5.elf"]);
    expect(names("0.14")).toEqual(["ftpsrv.elf"]);
    expect(names("payload-dev")).toEqual(["ftpsrv.elf"]);
  });

  it("wants every word, in any order", () => {
    expect(filterSaved(list, "fpkg kstuff").map((p) => p.filename)).toEqual(["kstuff-1.13-fpkg.elf"]);
    expect(filterSaved(list, "kstuff ftp")).toEqual([]);
  });

  it("is offered for more than eight payloads", () => {
    expect(SAVED_FILTER_MIN).toBe(8);
  });
});
