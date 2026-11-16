import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { t } from "../strings";
import { ConvertView, progressLine, type ConvertViewProps } from "./Convert";
import type { ConvertRun, Inspection } from "./model";

const INFO: Inspection = {
  path: "/data/games/PPSA01234",
  format: "folder",
  size: 2_000_000_000,
  files: 12,
  dataBytes: 2_000_000_000,
  titleId: "PPSA01234",
  contentId: null,
  title: "A Game",
  contentVersion: "01.000.000",
  masterVersion: null,
  packed: "zstd",
  hasEboot: true,
  warnings: [],
  zstdRuntime: "0.4.1",
};

function props(over: Partial<ConvertViewProps> = {}): ConvertViewProps {
  return {
    source: "/data/games/PPSA01234",
    inspected: { phase: "done", info: INFO },
    format: "exfat",
    dest: "/data/games/PPSA01234.exfat",
    free: 5_000_000_000,
    exists: false,
    naming: false,
    zstd: false,
    level: 5,
    inner: "exfat",
    ffpfscLevel: "balanced",
    pkgLevel: "fast",
    verify: "fast",
    run: null,
    onPickSource: vi.fn(),
    onPickDest: vi.fn(),
    onFormat: vi.fn(),
    onKeepBoth: vi.fn(),
    onZstd: vi.fn(),
    onLevel: vi.fn(),
    onInner: vi.fn(),
    onFfpfscLevel: vi.fn(),
    onPkgLevel: vi.fn(),
    onVerify: vi.fn(),
    onStart: vi.fn(),
    onStop: vi.fn(),
    ...over,
  };
}

const run = (over: Partial<ConvertRun> = {}): ConvertRun => ({
  source: "/data/games/PPSA01234",
  dest: "/data/games/PPSA01234.exfat",
  job: "j",
  bytesDone: 0,
  bytesTotal: 0,
  message: null,
  stopping: false,
  end: null,
  ...over,
});

const html = (p: ConvertViewProps) => renderToStaticMarkup(<ConvertView {...p} />);
const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/'/g, "&#x27;").replace(/"/g, "&quot;");

describe("Convert", () => {
  it("shows what the game is: title, size, format, and that it reads through AMPR", () => {
    const h = html(props());
    expect(h).toContain("A Game");
    expect(h).toContain(t("cw_convert_format_folder"));
    expect(h).toContain('data-convert-ampr="zstd"');
    expect(h).toContain(esc(t("cw_convert_ampr_yes", { kind: "zstd" })));
    expect(h).toMatch(/data-action="convert-go"(?![^>]*disabled)/);
  });

  it("a game in an archive is labelled so and says it is read in place", () => {
    expect(html(props())).not.toContain("data-convert-archive");
    const h = html(props({ inspected: { phase: "done", info: { ...INFO, format: "7z", path: "/data/g.7z" } } }));
    expect(h).toContain(t("cw_convert_format_7z"));
    expect(h).toContain("data-convert-archive");
    expect(h).toContain(esc(t("cw_convert_archive_note")));
  });

  it("a .ffpfsc offers the image inside it and its compression, and says what it costs", () => {
    expect(html(props())).not.toContain("data-convert-ffpfsc");
    const h = html(props({ format: "ffpfsc", dest: "/data/games/PPSA01234.ffpfsc", inner: "ffpkg", ffpfscLevel: "smallest" }));
    for (const f of ["exfat", "ffpkg", "ffpfs"]) expect(h).toContain(`data-action="convert-inner-${f}"`);
    expect(h).toMatch(/aria-pressed="true"[^>]*data-action="convert-inner-ffpkg"/);
    expect(h).toMatch(/aria-pressed="true"[^>]*data-action="convert-ffpfsc-level-smallest"/);
    expect(h).toContain(esc(t("cw_convert_ffpfsc_note")));
    expect(progressLine(run({ message: "convert_stage_compress" }))).toBe(t("cw_convert_stage_compress"));
  });

  it("a .pkg offers its levels, says what it costs, and has no zstd packs", () => {
    expect(html(props())).not.toContain("data-convert-pkg");
    const h = html(props({ format: "pkg", dest: "/data/games/PPSA01234.pkg", pkgLevel: "stored", zstd: true }));
    for (const l of ["stored", "fast", "balanced"]) expect(h).toContain(`data-action="convert-pkg-level-${l}"`);
    expect(h).toMatch(/aria-pressed="true"[^>]*data-action="convert-pkg-level-stored"/);
    expect(h).toContain(esc(t("cw_convert_pkg_note")));
    expect(h).not.toContain('data-action="convert-zstd"');
    expect(progressLine(run({ message: "convert_stage_check" }))).toBe(t("cw_convert_stage_read"));
  });

  it("offers the six outputs, and the zstd levels only with zstd on", () => {
    const off = html(props());
    for (const f of ["exfat", "ffpkg", "ffpfs", "ffpfsc", "pkg", "folder"]) expect(off).toContain(`data-action="convert-format-${f}"`);
    expect(off).not.toContain("convert-level-19");
    const on = html(props({ zstd: true, level: 19 }));
    expect(on).toMatch(/aria-pressed="true"[^>]*data-action="convert-level-19"/);
  });

  it("without the zstd runtime on the console, zstd is off and says how to get it", () => {
    const none = { phase: "done" as const, info: { ...INFO, zstdRuntime: null } };
    const h = html(props({ inspected: none, zstd: true, level: 19 }));
    expect(h).toMatch(/aria-checked="false"[^>]*data-action="convert-zstd"[^>]*disabled/);
    expect(h).toContain('data-convert-zstd-runtime="missing"');
    expect(h).toContain(esc(t("cw_convert_zstd_no_runtime")));
    expect(h).not.toContain("convert-level-19");
    // With it: no sentence, the switch works.
    const has = html(props({ zstd: true }));
    expect(has).not.toContain("data-convert-zstd-runtime");
    expect(has).toMatch(/aria-checked="true"[^>]*data-action="convert-zstd"(?![^>]*disabled)/);
  });

  it("a name already there blocks Convert and offers Keep both", () => {
    const h = html(props({ exists: true }));
    expect(h).toContain('data-convert-blocked="exists"');
    expect(h).toContain('data-action="convert-keep-both"');
    expect(h).toMatch(/data-action="convert-go"[^>]*disabled/);
  });

  it("while it runs: progress and Stop, and the choices are held", () => {
    const r = run({ bytesDone: 50, bytesTotal: 100, message: "convert_stage_write" });
    const h = html(props({ run: r }));
    expect(h).toContain("data-convert-progress");
    expect(h).toContain('data-action="convert-stop"');
    expect(h).toMatch(/data-action="convert-format-ffpkg"[^>]*disabled/);
    expect(progressLine(r)).toContain(t("cw_convert_stage_write"));
    expect(progressLine(r)).toContain(t("cw_home_percent", { n: 50 }));
    expect(progressLine(run({ stopping: true }))).toBe(t("cw_files_batch_cancelling"));
    expect(html(props({ run: run({ stopping: true }) }))).not.toContain('data-action="convert-stop"');
  });

  it("an ended conversion leaves its line", () => {
    const text = t("cw_convert_failed", { reason: t("cw_convert_err_not_built_yet") });
    const h = html(props({ run: run({ end: { tone: "error", text } }) }));
    expect(h).toContain('data-convert-end="error"');
    expect(h).toContain(esc(text));
  });

  it("a game that could not be read says why, and no output is offered", () => {
    const h = html(props({ inspected: { phase: "failed", error: t("cw_convert_err_unsupported_source") } }));
    expect(h).toContain("data-convert-inspect-error");
    expect(h).not.toContain("convert-go");
  });
});
