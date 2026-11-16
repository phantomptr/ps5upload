import { afterEach, describe, expect, it, vi } from "vitest";
import { ApiError } from "../api";
import type { OpEnd, OpStatus } from "../files/opJob";
import en from "../locales/en.json";
import { t } from "../strings";
import { freeName } from "../files/paste";
import {
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
  isConvertImage,
  isConvertSource,
  outputName,
  parentOf,
  parseInspection,
  resetConvertForTest,
  stageText,
  startBody,
  startConvert,
  stopConvert,
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
    expect(isConvertImage("a.ffpfs")).toBe(true);
    expect(isConvertImage("a.ffpfsc")).toBe(true);
    expect(isConvertImage("a.pkg")).toBe(false);
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
    expect(isConvertImage("g.zip")).toBe(false);
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
