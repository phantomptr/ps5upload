import { afterEach, describe, expect, it, vi } from "vitest";
import { ApiError } from "../api";
import type { OpEnd, OpStatus } from "../files/opJob";
import en from "../locales/en.json";
import { t } from "../strings";
import { freeName } from "../files/paste";
import {
  bytesToWrite,
  canUnpack,
  CONVERT_CAUSES,
  convertCauseText,
  convertErrorText,
  convertFailureText,
  currentConvert,
  dismissConvert,
  endOf,
  fitFfpfscName,
  inspect,
  isArchiveFormat,
  isConvertSource,
  outputName,
  parentOf,
  parseInspection,
  resetConvertForTest,
  stageText,
  startBody,
  startConvert,
  stopConvert,
  unpackFor,
  unpackNeedsEmulator,
  type JobDeps,
} from "./model";

const EN = en as Record<string, string>;

// The helper's and the converter's sources, and this section's own, as text.
const causeSources = import.meta.glob(
  [
    "../../../../payload/src/console_convert.c",
    "../../../../engine/crates/ps5upload-convert/src/job.rs",
    "../../../../engine/crates/ps5upload-convert/src/lib.rs",
  ],
  { query: "?raw", import: "default", eager: true },
) as Record<string, string>;
const pageSources = import.meta.glob(["./Convert.tsx", "./model.ts"], { query: "?raw", import: "default", eager: true }) as Record<
  string,
  string
>;

const status = (over: Partial<OpStatus> = {}): OpStatus => ({
  job: "j",
  state: "running",
  files_done: 0,
  files_total: 0,
  bytes_done: 0,
  bytes_total: 0,
  message: null,
  result: null,
  code: null,
  ...over,
});

describe("convert causes", () => {
  it("every cause the helper, the converter and the start route end with has its own sentence", () => {
    // The tokens in the C and Rust sources, so a new one cannot ship as a raw code.
    const sources = Object.values(causeSources);
    expect(sources).toHaveLength(3);
    const tokens = new Set<string>();
    for (const s of sources) for (const m of s.matchAll(/"(convert_[a-z_]+)"/g)) tokens.add(m[1]);
    // Not causes: the running job's line and the prefix the op matches with strncmp.
    tokens.delete("convert_starting");
    tokens.delete("convert_stage_");
    for (const tk of ["bad_format", "bad_verify", "bad_zstd"]) tokens.add(tk);
    expect(tokens.size).toBeGreaterThan(10);
    for (const tk of tokens) {
      expect(CONVERT_CAUSES[tk], tk).toBeDefined();
      expect(EN[CONVERT_CAUSES[tk]], `${tk} → ${CONVERT_CAUSES[tk]}`).toBeTruthy();
    }
  });

  it("the converter's not-built-yet ending reads as a plain sentence", () => {
    expect(convertCauseText("convert_not_built_yet")).toBe(
      "Converting on the PS5 isn't finished yet: this build only reads the game through.",
    );
  });

  it("reads the token up to its first colon or space", () => {
    expect(convertCauseText("convert_write_failed: request")).toBe(t("cw_convert_err_failed"));
    expect(convertCauseText("something_else")).toBeNull();
  });

  it("a route's refusal and an unknown failure", () => {
    expect(convertErrorText(new ApiError(400, "bad_zstd"))).toBe(t("cw_convert_err_bad_request"));
    expect(convertErrorText(new ApiError(403, "path_not_allowed"))).toBe(t("cw_files_err_denied"));
    expect(convertErrorText(new ApiError(401, "unauthorized"))).toBeNull();
  });

  it("every key the page uses is in English", () => {
    const src = Object.values(pageSources).join("\n");
    expect(src).toContain("cw_convert_title");
    for (const m of src.matchAll(/"(cw_[a-z0-9_]+)"/g)) expect(EN[m[1]], m[1]).toBeTruthy();
  });
});

describe("names and requests", () => {
  it("names the output after the source", () => {
    expect(outputName("/data/games/PPSA01234", "exfat")).toBe("PPSA01234.exfat");
    expect(outputName("/mnt/usb0/Astro.ffpkg", "folder")).toBe("Astro");
    expect(outputName("/mnt/usb0/Astro.EXFAT/", "ffpfs")).toBe("Astro.ffpfs");
    expect(parentOf("/mnt/usb0/Astro.ffpkg")).toBe("/mnt/usb0");
    expect(isConvertSource("a.ffpfs")).toBe(true);
    expect(isConvertSource("a.ffpfsc")).toBe(true);
    expect(isConvertSource("a.pkg")).toBe(false);
    // A .ffpfsc's name fits the 63 bytes ShadowMountPlus mounts, cut at a character.
    expect(outputName("/data/Astro.ffpfsc", "exfat")).toBe("Astro.exfat");
    expect(outputName("/data/Astro", "ffpfsc")).toBe("Astro.ffpfsc");
    const long = outputName(`/data/${"é".repeat(40)}`, "ffpfsc");
    expect(new TextEncoder().encode(long).length).toBeLessThanOrEqual(63);
    expect(long).toBe(`${"é".repeat(28)}.ffpfsc`);
  });

  it("keeps a Keep both .ffpfsc name within 63 bytes, cutting the stem", async () => {
    const bytes = (s: string) => new TextEncoder().encode(s).length;
    const full = outputName(`/data/${"é".repeat(40)}`, "ffpfsc"); // 63 bytes already
    const taken = new Set([`/d/${full}`]);
    const next = await freeName("/d", full, false, async (p) => taken.has(p), fitFfpfscName);
    expect(bytes(next)).toBeLessThanOrEqual(63);
    expect(next).toBe(`${"é".repeat(26)} (2).ffpfsc`);
    // The fitted name is the one checked: a taken "(2)" moves on to "(3)".
    taken.add(`/d/${next}`);
    expect(await freeName("/d", full, false, async (p) => taken.has(p), fitFfpfscName)).toBe(
      `${"é".repeat(26)} (3).ffpfsc`,
    );
    // Short names and other formats are left as they are.
    expect(fitFfpfscName("Astro (2).ffpfsc")).toBe("Astro (2).ffpfsc");
    const longExfat = `${"a".repeat(70)} (2).exfat`;
    expect(fitFfpfscName(longExfat)).toBe(longExfat);
  });

  it("offers archives as sources and names the output after the archive", () => {
    for (const n of ["g.zip", "G.7Z", "g.part1.rar"]) expect(isConvertSource(n), n).toBe(true);
    expect(isConvertSource("a.ffpfsc")).toBe(true);
    expect(isConvertSource("a.tar")).toBe(false);
    expect(outputName("/data/Astro Bot.zip", "exfat")).toBe("Astro Bot.exfat");
    expect(outputName("/data/Astro.part1.rar", "folder")).toBe("Astro");
    expect(outputName("/data/Astro.7z", "ffpfsc")).toBe("Astro.ffpfsc");
    expect(isArchiveFormat("7z") && isArchiveFormat("zip") && !isArchiveFormat("folder")).toBe(true);
    expect(convertCauseText("convert_rar_unsupported")).toBe(t("cw_convert_err_rar"));
  });

  it("sends a .pkg's level with it, and never zstd", () => {
    const base = { source: "/data/g", dest: "/data/g.pkg", format: "pkg" as const, level: 5 as const, verify: "fast" as const };
    expect(startBody({ ...base, zstd: true, pkgLevel: "balanced" })).toEqual({
      path: "/data/g",
      dest: "/data/g.pkg",
      format: "pkg",
      zstd: false,
      pkg_level: "balanced",
      verify: "fast",
    });
    expect(startBody({ ...base, zstd: false })).toMatchObject({ pkg_level: "fast" });
    expect(startBody({ ...base, format: "exfat", zstd: false, pkgLevel: "stored" })).not.toHaveProperty("pkg_level");
    expect(outputName("/data/Astro", "pkg")).toBe("Astro.pkg");
  });

  it("sends the unpack only with a .pkg, and the emulator left out only with the unpack", () => {
    const base = { source: "/data/g", dest: "/data/g.pkg", format: "pkg" as const, zstd: false, level: 5 as const, verify: "fast" as const };
    // Not asked: neither word goes (an older helper, and this one, then package the game as it is).
    const asis = startBody(base);
    expect(asis).not.toHaveProperty("unpack_packs");
    expect(asis).not.toHaveProperty("leave_out_ampr_emu");
    expect(startBody({ ...base, unpack: true })).toEqual({
      path: "/data/g",
      dest: "/data/g.pkg",
      format: "pkg",
      zstd: false,
      pkg_level: "fast",
      unpack_packs: true,
      verify: "fast",
    });
    expect(startBody({ ...base, unpack: true, leaveOutEmulator: true })).toMatchObject({ unpack_packs: true, leave_out_ampr_emu: true });
    const packs = startBody({ ...base, unpack: false, leaveOutEmulator: true });
    expect(packs).not.toHaveProperty("unpack_packs");
    expect(packs).not.toHaveProperty("leave_out_ampr_emu");
    const image = startBody({ ...base, format: "exfat", dest: "/data/g.exfat", unpack: true, leaveOutEmulator: true });
    expect(image).not.toHaveProperty("unpack_packs");
    expect(image).not.toHaveProperty("leave_out_ampr_emu");
  });

  it("the unpack applies to a .pkg of a packed game, and says when the emulator can't stay", () => {
    const packed = parseInspection({ path: "/g", format: "folder", data_bytes: 20, packed: "zstd", unpacked_bytes: 50, unpack_keeps_emulator: false });
    expect(packed).toMatchObject({ unpackedBytes: 50, unpackKeepsEmulator: false });
    const plain = parseInspection({ path: "/g", format: "folder", data_bytes: 20 });
    expect(plain).toMatchObject({ unpackedBytes: null, unpackKeepsEmulator: null });
    // What is not a size or a yes/no reads as absent, never as 0 or false.
    const odd = parseInspection({ path: "/g", format: "folder", packed: "lz4", unpacked_bytes: "50", unpack_keeps_emulator: 0 });
    expect(odd).toMatchObject({ unpackedBytes: null, unpackKeepsEmulator: null });
    expect([canUnpack(packed), canUnpack(plain), canUnpack(odd), canUnpack(null)]).toEqual([true, false, false, false]);

    const both = { unpack: true, leaveOutEmulator: true };
    expect(unpackFor("pkg", packed, both)).toEqual(both);
    expect(unpackFor("pkg", packed, { unpack: false, leaveOutEmulator: true })).toEqual({ unpack: false, leaveOutEmulator: false });
    for (const none of [unpackFor("exfat", packed, both), unpackFor("pkg", plain, both), unpackFor("pkg", odd, both)]) {
      expect(none).toEqual({ unpack: false, leaveOutEmulator: false });
    }

    const keep = { unpack: true, leaveOutEmulator: false };
    expect(unpackNeedsEmulator(packed, keep)).toBe(true);
    expect(unpackNeedsEmulator(packed, both)).toBe(false);
    expect(unpackNeedsEmulator(packed, { unpack: false, leaveOutEmulator: false })).toBe(false);
    const fine = parseInspection({ path: "/g", format: "folder", packed: "lz4", unpacked_bytes: 50, unpack_keeps_emulator: true });
    expect(unpackNeedsEmulator(fine, keep)).toBe(false);
    // ampr_emu that the app put on this PS5: the helper says the emulator can stay, and the
    // keep choice builds (its version is extra; an older page reads only the yes/no).
    const onConsole = parseInspection({ path: "/g", format: "folder", packed: "zstd", unpacked_bytes: 50, unpack_keeps_emulator: true, ampr_emu: "0.4.2.1" });
    expect(unpackNeedsEmulator(onConsole, keep)).toBe(false);
    // The refusal names the way out: the app's button.
    expect(t("cw_convert_unpack_needs_emulator_v2")).toContain("Put the AMPR emulator on this PS5");
    // The converter's own refusal of it reads as the same sentence the page shows beforehand.
    expect(convertFailureText("convert_unpack_needs_emulator")).toBe(t("cw_convert_unpack_needs_emulator_v2"));
    expect(convertFailureText("convert_not_packed")).toBe(t("cw_convert_err_not_packed"));

    expect(packed && bytesToWrite(packed, keep)).toBe(50);
    expect(packed && bytesToWrite(packed, { unpack: false, leaveOutEmulator: false })).toBe(20);
  });

  it("sends the image inside and its compression only with a .ffpfsc", () => {
    const base = { source: "/data/g", dest: "/data/g.ffpfsc", zstd: false, level: 5 as const, verify: "fast" as const };
    expect(startBody({ ...base, format: "ffpfsc", inner: "ffpkg", ffpfscLevel: "smallest" })).toMatchObject({
      format: "ffpfsc",
      inner: "ffpkg",
      ffpfsc_level: "smallest",
    });
    expect(startBody({ ...base, format: "ffpfsc" })).toMatchObject({ inner: "exfat", ffpfsc_level: "balanced" });
    const plain = startBody({ ...base, format: "exfat", inner: "ffpkg", ffpfscLevel: "fast" });
    expect(plain).not.toHaveProperty("inner");
    expect(plain).not.toHaveProperty("ffpfsc_level");
  });

  it("sends the zstd level only with zstd", () => {
    const base = { source: "/data/g", dest: "/data/g.exfat", format: "exfat" as const, level: 19 as const, verify: "full" as const };
    expect(startBody({ ...base, zstd: true })).toEqual({ path: "/data/g", dest: "/data/g.exfat", format: "exfat", zstd: true, zstd_level: 19, verify: "full" });
    expect(startBody({ ...base, zstd: false })).not.toHaveProperty("zstd_level");
  });

  it("parses an inspection and refuses what is not one", () => {
    const i = parseInspection({ path: "/data/g", format: "folder", size: 10, files: 2, data_bytes: 9, title: "Game", title_id: "PPSA00001", packed: "zstd", has_eboot: true, warnings: ["x", 3] });
    expect(i).toMatchObject({ title: "Game", titleId: "PPSA00001", packed: "zstd", hasEboot: true, warnings: ["x"], dataBytes: 9, contentId: null });
    expect(parseInspection({ format: "folder" })).toBeNull();
    expect(parseInspection(null)).toBeNull();
    // The zstd runtime the helper found, or none (also from a helper that does not say).
    expect(i?.zstdRuntime).toBeNull();
    expect(parseInspection({ path: "/g", format: "folder", zstd_runtime: "0.4.1" })?.zstdRuntime).toBe("0.4.1");
    expect(parseInspection({ path: "/g", format: "folder", zstd_runtime: null })?.zstdRuntime).toBeNull();
  });

  it("a zstd build refused for want of the runtime says how to get it", () => {
    expect(convertFailureText("convert_no_zstd_runtime")).toBe(t("cw_convert_zstd_no_runtime"));
  });

  it("stage lines", () => {
    expect(stageText("convert_starting")).toBe(t("cw_convert_stage_starting"));
    expect(stageText("convert_stage_read")).toBe(t("cw_convert_stage_read"));
    expect(stageText("convert_stage_pack")).toBe(t("cw_convert_stage_pack"));
    expect(stageText("convert_stage_plan")).toBe(t("cw_convert_stage_plan"));
    expect(stageText("convert_stage_write")).toBe(t("cw_convert_stage_write"));
    expect(stageText("convert_stage_verify")).toBe(t("cw_convert_stage_verify"));
    expect(stageText("convert_stage_other_thing")).toBe(t("cw_convert_stage_other"));
    expect(stageText(null)).toBeNull();
  });
});

describe("jobs", () => {
  afterEach(() => resetConvertForTest());

  const deps = (end: OpEnd, onWatch?: (o: Parameters<JobDeps["watch"]>[1]) => void): JobDeps => ({
    start: vi.fn(async () => "job1"),
    watch: vi.fn(async (_job: string, o?: Parameters<JobDeps["watch"]>[1]) => {
      onWatch?.(o);
      return end;
    }) as unknown as JobDeps["watch"],
  });

  it("inspect: the result, or the converter's cause as a sentence", async () => {
    const ok = await inspect("/data/g", deps({ end: "done", status: status({ state: "done", result: '{"path":"/data/g","format":"exfat","size":1}' }) }));
    expect(ok).toMatchObject({ ok: true, info: { format: "exfat" } });
    const bad = await inspect("/data/g", deps({ end: "failed", status: status({ state: "failed", message: "convert_unsupported_source" }) }));
    expect(bad).toEqual({ ok: false, error: t("cw_convert_err_unsupported_source") });
  });

  it("inspect: another source picked mid-check stops the first, and the second starts once it ended", async () => {
    const order: string[] = [];
    let endFirst: (e: OpEnd) => void = () => undefined;
    let first = true;
    const d: JobDeps = {
      start: vi.fn(async (_route: string, body?: object) => {
        order.push(`start ${(body as { path: string }).path}`);
        return (body as { path: string }).path;
      }) as unknown as JobDeps["start"],
      watch: vi.fn((job: string, o?: Parameters<JobDeps["watch"]>[1]) => {
        if (first) {
          first = false;
          // the first check runs until it is stopped (watchOp asks the helper to cancel it)
          return new Promise<OpEnd>((res) => {
            endFirst = (e) => {
              order.push(`end ${job} stopped=${o?.stopped?.()}`);
              res(e);
            };
          });
        }
        return Promise.resolve({ end: "done", status: status({ state: "done", result: `{"path":"${job}","format":"exfat","size":1}` }) } as OpEnd);
      }) as unknown as JobDeps["watch"],
    };
    let firstLive = true;
    const a = inspect("/data/a", d, () => !firstLive);
    const b = inspect("/data/b", d);
    await vi.waitFor(() => expect(d.watch).toHaveBeenCalledTimes(1));
    firstLive = false;
    // the second has not started while the first runs: the helper takes one converter at a time
    expect(order).toEqual(["start /data/a"]);
    endFirst({ end: "cancelled", status: status({ state: "failed" }) });
    expect(await a).toEqual({ ok: false, error: null });
    expect(await b).toMatchObject({ ok: true, info: { path: "/data/b" } });
    expect(order).toEqual(["start /data/a", "end /data/a stopped=true", "start /data/b"]);
    // a check stopped before its turn never starts
    expect(await inspect("/data/c", d, () => true)).toEqual({ ok: false, error: null });
    expect(order).toHaveLength(3);
  });

  it("a conversion: progress, then the not-built-yet ending", async () => {
    let seen: string | null = null;
    const d = deps({ end: "failed", status: status({ state: "failed", message: "convert_not_built_yet" }) }, (o) => {
      o?.onStatus?.(status({ bytes_done: 5, bytes_total: 10, message: "convert_stage_read" }));
      seen = currentConvert()?.message ?? null;
    });
    await startConvert(startBody({ source: "/data/g", dest: "/data/g.exfat", format: "exfat", zstd: false, level: 5, verify: "fast" }), d);
    expect(seen).toBe("convert_stage_read");
    expect(currentConvert()?.bytesDone).toBe(5);
    expect(currentConvert()?.end).toEqual({ tone: "error", text: t("cw_convert_failed", { reason: t("cw_convert_err_not_built_yet") }) });
    dismissConvert();
    expect(currentConvert()).toBeNull();
  });

  it("Stop asks the job to stop; a stopped job reads as stopped", async () => {
    let stopped: boolean | undefined;
    const d = deps({ end: "cancelled", status: status({ state: "failed", message: "convert_cancelled" }) }, (o) => {
      stopConvert();
      stopped = o?.stopped?.();
    });
    await startConvert(startBody({ source: "/data/g", dest: "/data/g", format: "folder", zstd: false, level: 5, verify: "fast" }), d);
    expect(stopped).toBe(true);
    expect(currentConvert()?.end).toEqual({ tone: "info", text: t("cw_convert_stopped") });
  });

  it("a refused start says why; a 401 drops the run", async () => {
    const refused: JobDeps = { start: vi.fn(async () => Promise.reject(new ApiError(409, "convert_busy"))), watch: vi.fn() as unknown as JobDeps["watch"] };
    await startConvert(startBody({ source: "/data/g", dest: "/data/g.ffpkg", format: "ffpkg", zstd: false, level: 5, verify: "fast" }), refused);
    expect(currentConvert()?.end?.text).toBe(t("cw_convert_failed", { reason: t("cw_convert_err_busy") }));
    resetConvertForTest();
    const gone: JobDeps = { start: vi.fn(async () => Promise.reject(new ApiError(401, "unauthorized"))), watch: vi.fn() as unknown as JobDeps["watch"] };
    await startConvert(startBody({ source: "/data/g", dest: "/data/g.ffpkg", format: "ffpkg", zstd: false, level: 5, verify: "fast" }), gone);
    expect(currentConvert()).toBeNull();
  });

  it("a finished job names what it wrote", () => {
    expect(endOf({ end: "done", status: status({ state: "done", result: '{"path":"/data/x.exfat"}' }) }, "/data/y")).toEqual({
      tone: "info",
      text: t("cw_convert_done", { path: "/data/x.exfat" }),
    });
    expect(endOf({ end: "gone" }, "/data/y").text).toBe(t("cw_convert_lost"));
  });
});
