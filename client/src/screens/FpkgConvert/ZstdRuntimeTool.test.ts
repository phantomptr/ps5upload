import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import {
  consoleOf,
  runtimeNow,
  smpWarning,
  zstdRuntimeOffered,
} from "./ZstdRuntimeTool";
import type { FpkgInspection } from "../../api/fpkg";
import type { Translator } from "../../state/lang";

const tr = ((_k: string, a?: unknown, b?: string) => {
  const s = typeof a === "string" ? a : (b ?? _k);
  const vars = (typeof a === "object" && a) || {};
  return s.replace(/\{(\w+)\}/g, (_, k: string) =>
    String((vars as Record<string, unknown>)[k] ?? ""),
  );
}) as Translator;

const packed = (zstd_runtime: FpkgInspection["zstd_runtime"]) =>
  ({ zstd_runtime }) as FpkgInspection;
const OLD = packed({
  module: "fakelib/libSceAmpr.sprx",
  kind: "ampr-emu-ng",
  version: "0.3.2",
  outdated: true,
});
const NEW = packed({
  module: "fakelib/libSceAmpr.sprx",
  kind: "ampr-emu-ng",
  version: "0.4.1",
  outdated: false,
  capped: false,
});
const CAPPED = packed({
  module: "fakelib/libSceAmpr.sprx",
  kind: "ampr-emu-ng",
  version: "0.4.0",
  outdated: false,
  capped: true,
});

describe("Update the zstd runtime", () => {
  it("is offered for a packed game with an old runtime: a folder anywhere, an image on the PS5", () => {
    expect(zstdRuntimeOffered("/games/PPSA21567", OLD)).toBe(true);
    expect(
      zstdRuntimeOffered("ps5://192.0.2.7/data/homebrew/PPSA21567.exfat", OLD),
    ).toBe(true);
    // An image here is converted again instead.
    expect(zstdRuntimeOffered("/games/PPSA21567.exfat", OLD)).toBe(false);
    expect(zstdRuntimeOffered("/games/PPSA21567", NEW)).toBe(false);
    expect(zstdRuntimeOffered("/games/PPSA21567", null)).toBe(false);
    // 0.4.0 starts most games and refuses very large ones: offered too.
    expect(zstdRuntimeOffered("/games/PPSA21567", CAPPED)).toBe(true);
    expect(zstdRuntimeOffered("/games/PPSA21567.exfat", CAPPED)).toBe(false);
  });

  it("says why the game may not start", () => {
    expect(runtimeNow(OLD, tr)).toContain("ampr-emu-ng 0.3.2");
    expect(runtimeNow(OLD, tr)).toContain("ASTRO BOT");
    expect(
      runtimeNow(
        packed({ kind: "ampr_emu", version: "0.4.2.1", outdated: true }),
        tr,
      ),
    ).toContain("reads LZ4 packs only");
    expect(runtimeNow(CAPPED, tr)).toContain("ampr-emu-ng 0.4.0");
    expect(runtimeNow(CAPPED, tr)).toContain("over the 48 MiB");
    expect(runtimeNow(NEW, tr)).toBeNull();
  });

  it("warns only when ShadowMount+ swaps in a runtime that breaks zstd games", () => {
    expect(
      smpWarning(
        {
          updates_on: true,
          path: "/data/shadowmount/emus/libSceAmpr.sprx",
          kind: "ampr_emu",
          version: "0.4.2.1",
          breaks_zstd: true,
        },
        tr,
      ),
    ).toContain("ampr_emu 0.4.2.1");
    expect(
      smpWarning(
        {
          updates_on: false,
          path: "/data/shadowmount/emus/libSceAmpr.sprx",
          kind: "ampr_emu",
          breaks_zstd: false,
        },
        tr,
      ),
    ).toBeNull();
    expect(smpWarning(null, tr)).toBeNull();
  });

  it("reads the console from a ps5:// source", () => {
    expect(consoleOf("ps5://192.0.2.7/data/x")).toBe("192.0.2.7");
    expect(consoleOf("/data/x")).toBeNull();
  });
});
