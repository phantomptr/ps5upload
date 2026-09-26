import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("../../state/lang", () => ({
  useTr: () =>
    (key: string, vars?: Record<string, string | number>, fallback?: string) => {
      let s = fallback ?? key;
      for (const [k, v] of Object.entries(vars ?? {})) s = s.replace(`{${k}}`, String(v));
      return s;
    },
}));

import type { Pipeline } from "../../state/fpkgConversion";
import { RunCard, deleteLabel } from "./RunCard";

// No DOM here (vitest in node, no testing-library): render to markup and check what a user
// would see and could press.
const noop = () => {};
const actions = {
  onConvert: noop,
  onConvertInstall: noop,
  onCompress: noop,
  onCancel: noop,
  onRetryInstall: noop,
  onLaunch: noop,
  onShowFolder: noop,
  onInstallAgain: noop,
  onDelete: noop,
  onAnother: noop,
};

function html(
  pipeline: Pipeline,
  opts: { canInstall?: boolean; isImage?: boolean; title?: string; sourceBytes?: number } = {},
) {
  return renderToStaticMarkup(
    <RunCard
      pipeline={pipeline}
      installTask={null}
      host="10.0.0.2"
      canInstall={opts.canInstall ?? true}
      isImage={opts.isImage ?? false}
      deleteArmed={false}
      title={opts.title ?? null}
      sourceBytes={opts.sourceBytes ?? 0}
      {...actions}
    />,
  );
}

/** The button whose text contains `text` — `{ disabled }` from its real attribute (its class
 *  list also names Tailwind's `disabled:` variants) — or null when there is none. */
function button(out: string, text: string): { disabled: boolean } | null {
  const re = /<button([^>]*)>([\s\S]*?)<\/button>/g;
  for (let m = re.exec(out); m; m = re.exec(out)) {
    const shown = m[2].replace(/<[^>]+>/g, "").replace(/&amp;/g, "&");
    if (shown.includes(text)) return { disabled: /\sdisabled(=""|\s|$)/.test(m[1]) };
  }
  return null;
}

const done = {
  phase: "done" as const,
  source: "/g",
  host: "10.0.0.2",
  packagePath: "/out/a.pkg",
  packageBytes: 1024,
  convertMs: 60_000,
  installMs: 30_000,
  stageMs: {},
  deleted: false,
  titleId: "PPSA17221",
};

describe("RunCard", () => {
  it("offers only Convert when no console is connected", () => {
    const out = html({ phase: "idle" }, { canInstall: false });
    expect(button(out, "Convert only")?.disabled).toBe(false);
    expect(button(out, "Convert & install")?.disabled).toBe(true);
    expect(out).toContain("Connect to a PS5 to install");
  });

  it("offers Convert & install with a console, and .ffpfsc only for an image", () => {
    expect(button(html({ phase: "idle" }), "Convert & install")?.disabled).toBe(false);
    expect(button(html({ phase: "idle" }), "Compress to .ffpfsc")).toBeNull();
    expect(button(html({ phase: "idle" }, { isImage: true }), "Compress to .ffpfsc")).not.toBeNull();
  });

  it("lists the stages while running, with Cancel during the build", () => {
    const out = html({
      phase: "running",
      mode: "convert-install",
      source: "/g",
      host: "10.0.0.2",
      stage: "compress",
      stageDone: 5,
      stageTotal: 10,
      startedMs: 0,
      stageStartedMs: 0,
      stageMs: { check: 1000, plan: 2000 },
      jobId: "j",
      installTaskId: null,
      taskId: null,
      packagePath: null,
      titleId: null,
    });
    for (const label of ["Check source", "Plan package", "Compress", "Write package", "Verify", "Send to PS5", "Install on PS5"]) {
      expect(out).toContain(label);
    }
    expect(button(out, "Cancel")).not.toBeNull();
    expect(button(out, "Convert only")).toBeNull();
  });

  it("names the kept package and offers Retry install after an install failure", () => {
    const out = html({
      phase: "failed",
      mode: "convert-install",
      source: "/g",
      host: "10.0.0.2",
      stage: "install",
      message: "unreachable",
      packagePath: "/out/a.pkg",
      stageMs: {},
      titleId: null,
    });
    expect(out).toContain("built and kept");
    expect(out).toContain("unreachable");
    expect(button(out, "Retry install")).not.toBeNull();
  });

  it("offers no retry when the build itself failed", () => {
    const out = html({
      phase: "failed",
      mode: "convert",
      source: "/g",
      host: null,
      stage: "write",
      message: "disk full",
      packagePath: null,
      stageMs: {},
      titleId: null,
    });
    expect(button(out, "Retry install")).toBeNull();
    expect(out).not.toContain("built and kept");
  });

  it("shows the result actions after an install, and no Launch for Convert only", () => {
    const installed = html({ ...done, mode: "convert-install" });
    for (const name of ["Launch on PS5", "Install again", "Delete package", "Convert another game"]) {
      expect(button(installed, name)).not.toBeNull();
    }
    expect(button(html({ ...done, mode: "convert" }), "Launch on PS5")).toBeNull();
  });

  it("drops the package actions once the package is deleted", () => {
    const out = html({ ...done, mode: "convert-install", deleted: true });
    expect(out).toContain("Package deleted");
    for (const name of ["Install again", "Delete package", "Launch on PS5"]) {
      expect(button(out, name)).toBeNull();
    }
    expect(button(out, "Convert another game")).not.toBeNull();
  });

  it("names the game and how much the package saved", () => {
    const out = html({ ...done, mode: "convert-install", packageBytes: 1024 }, { title: "Minecraft", sourceBytes: 2048 });
    expect(out).toContain("Installed on PS5 — Minecraft");
    expect(out).toContain("50%");
  });

  it("shows speed and time left for a build stage and overall", () => {
    const out = html({
      phase: "running",
      mode: "convert",
      source: "/g",
      host: null,
      stage: "compress",
      stageDone: 50 * 1024 * 1024,
      stageTotal: 100 * 1024 * 1024,
      startedMs: Date.now() - 20_000,
      stageStartedMs: Date.now() - 10_000,
      stageMs: {},
      jobId: "j",
      installTaskId: null,
      taskId: null,
      packagePath: null,
      titleId: null,
    });
    expect(out).toMatch(/MiB\/s/);
    expect((out.match(/left/g) ?? []).length).toBeGreaterThanOrEqual(2);
  });

  it("asks before deleting: the first press arms, the second confirms", () => {
    expect(deleteLabel(false)).toBe("Delete package");
    expect(deleteLabel(true)).toBe("Confirm delete");
  });
});
