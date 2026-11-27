import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { EMPTY_QUEUE, type FolderSize, type MeasureQueue } from "../../api/folderSizes";
import type { Translator } from "../../state/lang";
import { FolderSizeCell, folderSizeState, folderSizeTitle, knownFolderBytes } from "./FolderSizeCell";

const tr = ((key: string, vars?: Record<string, string | number>, fallback?: string) => {
  let s = fallback ?? key;
  for (const [k, v] of Object.entries(vars ?? {})) s = s.replace(`{${k}}`, String(v));
  return s;
}) as Translator;

const NOW = Date.UTC(2026, 10, 27, 12, 0, 0);
const size = (over: Partial<FolderSize> = {}): FolderSize => ({
  bytes: 48.2 * 1024 ** 3,
  files: 1234,
  measured_at_ms: NOW - 2 * 86_400_000,
  source: "measured",
  stored: true,
  partial: false,
  ...over,
});
const queue = (over: Partial<MeasureQueue> = {}): MeasureQueue => ({ ...EMPTY_QUEUE, ...over });
const running = (path: string) => ({
  addr: "10.0.0.5",
  path,
  started_at_ms: NOW,
  files: 0,
  bytes: 0,
});

describe("folderSizeState", () => {
  it("unknown without a size, known with one", () => {
    expect(folderSizeState("/data/a", undefined, queue())).toEqual({ kind: "unknown" });
    const s = size();
    expect(folderSizeState("/data/a", s, queue())).toEqual({ kind: "known", size: s });
  });

  it("measuring, waiting and paused win over a size already known", () => {
    const s = size();
    expect(folderSizeState("/data/a", s, queue({ running: running("/data/a/") })).kind).toBe("running");
    expect(
      folderSizeState("/data/b", undefined, queue({ queued: [{ addr: "x", path: "/data/b" }] })).kind,
    ).toBe("queued");
    expect(
      folderSizeState("/data/a", s, queue({ running: running("/data/a"), paused: "busy" })).kind,
    ).toBe("paused");
    expect(
      folderSizeState("/data/b", s, queue({ queued: [{ addr: "x", path: "/data/b" }], paused: "busy" })).kind,
    ).toBe("paused");
    // Another folder's measuring says nothing about this one.
    expect(folderSizeState("/data/c", s, queue({ running: running("/data/a"), paused: "busy" })).kind).toBe(
      "known",
    );
  });

  it("a size sort reads the known size by name", () => {
    expect(knownFolderBytes({ a: size({ bytes: 7 }) }, "a")).toBe(7);
    expect(knownFolderBytes({ a: size() }, "b")).toBeUndefined();
    expect(knownFolderBytes(undefined, "a")).toBeUndefined();
  });
});

describe("FolderSizeCell", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(NOW);
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  const html = (state: Parameters<typeof FolderSizeCell>[0]["state"]) =>
    renderToStaticMarkup(<FolderSizeCell state={state} tr={tr} />);

  it("unknown is the dash it always was", () => {
    const h = html({ kind: "unknown" });
    expect(h).toContain('data-state="unknown"');
    expect(h).toContain(">—<");
  });

  it("known shows the size and how old it is, the age muted, with a tooltip", () => {
    const h = html({ kind: "known", size: size() });
    expect(h).toContain("48.2 GiB");
    expect(h).toMatch(/<span class="opacity-70"> · 2 days ago<\/span>/);
    expect(h).toContain("Measured by PS5Upload");
    expect(h).toContain("does not update by itself");
    expect(h).not.toContain("≥");
  });

  it("a partial size says it may be larger", () => {
    const h = html({ kind: "known", size: size({ partial: true }) });
    expect(h).toContain("≥ ");
    expect(h).toContain("could not be read");
  });

  it("measuring, waiting and paused each say so", () => {
    expect(html({ kind: "running" })).toContain("Measuring…");
    expect(html({ kind: "queued" })).toContain("Waiting…");
    const paused = html({ kind: "paused" });
    expect(paused).toContain("Paused");
    expect(paused).toContain("transfer or install");
  });

  it("the tooltip names where each size came from, and what kind of size it is", () => {
    expect(folderSizeTitle(size({ source: "space" }), tr, NOW)).toContain("What takes space?");
    expect(folderSizeTitle(size({ source: "transfer" }), tr, NOW)).toContain("transfer");
    expect(folderSizeTitle(size({ source: "cleanup" }), tr, NOW)).toContain("Clean up");
    expect(folderSizeTitle(size({ stored: true }), tr, NOW)).toContain("on the drive");
    expect(folderSizeTitle(size({ stored: false, files: null }), tr, NOW)).not.toMatch(/\d+ files\./);
    expect(folderSizeTitle(size({ stored: false }), tr, NOW)).toContain("1234 files.");
  });
});
