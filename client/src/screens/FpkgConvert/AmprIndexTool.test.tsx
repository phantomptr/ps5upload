import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import type { FpkgInspection } from "../../api/fpkg";
import { AmprIndexTool, amprIndexPlan, amprIndexStatus, type AmprIndexPlan } from "./AmprIndexTool";

const inspection = (kind: "ok" | "runtime" | "no_ampr" | null): FpkgInspection => ({
  source: "folder /g",
  files: 3,
  bytes: 3,
  planned_size: 3,
  checks: [],
  ampr_packs: kind ? { kind } : null,
});

const tr = ((_key: string, vars: Record<string, unknown> | undefined, fallback: string) =>
  fallback.replace(/\{(\w+)\}/g, (_m, n: string) => String(vars?.[n] ?? ""))) as Parameters<
  typeof amprIndexStatus
>[3];

describe("Rebuild AMPR index: what it does for each source", () => {
  it("follows where the game is and what it is", () => {
    const ampr = inspection("ok");
    const cases: [string, AmprIndexPlan][] = [
      ["/games/PPSA01234", "folder"],
      ["ps5://192.168.1.5/data/homebrew/PPSA01234", "console_folder"],
      ["/games/PPSA01234.exfat", "image"],
      ["/games/PPSA01234.ffpkg", "image"],
      ["/games/PPSA01234.ffpfsc", "compressed"],
      ["ps5://192.168.1.5/mnt/ext0/PPSA01234.ffpfsc", "compressed"],
      ["ps5://192.168.1.5/mnt/ext0/PPSA01234.exfat", "console_image"],
      // A package or an archive: a build makes a fresh index anyway.
      ["/games/PPSA01234.pkg", "hidden"],
      ["/games/PPSA01234.7z", "hidden"],
      ["remote://nas/games/PPSA01234", "hidden"],
      ["", "hidden"],
    ];
    for (const [source, plan] of cases) expect(amprIndexPlan(source, ampr), source).toBe(plan);
    // Only once a game is checked, and only for one that reads through libSceAmpr.
    expect(amprIndexPlan("/games/PPSA01234", null)).toBe("hidden");
    expect(amprIndexPlan("/games/PPSA01234", inspection("no_ampr"))).toBe("not_ampr");
    expect(amprIndexPlan("/games/PPSA01234", inspection(null))).toBe("not_ampr");
    expect(amprIndexPlan("/games/PPSA01234", inspection("runtime"))).toBe("folder");
  });
});

describe("Rebuild AMPR index: what it says", () => {
  const idle = { phase: "idle" } as const;

  it("says why it is off, and what a compressed image or one on the PS5 needs", () => {
    expect(amprIndexStatus("not_ampr", idle, false, tr)).toContain("reads its files through libSceAmpr");
    expect(amprIndexStatus("compressed", idle, false, tr)).toBeNull();
    expect(amprIndexStatus("compressed", idle, true, tr)).toBe(
      "This image is compressed and read-only: rebuild the index by converting it again (Workshop › Convert).",
    );
    expect(amprIndexStatus("console_image", idle, true, tr)).toContain("Copy it to this computer");
    expect(amprIndexStatus("folder", idle, false, tr)).toBeNull();
  });

  it("follows the job, then says what changed", () => {
    expect(amprIndexStatus("folder", { phase: "running", jobId: "j", stage: "list" }, false, tr)).toBe(
      "Listing the game's files…",
    );
    expect(amprIndexStatus("image", { phase: "running", jobId: "j", pct: 40 }, false, tr)).toBe(
      "Rebuilding the AMPR index… 40%",
    );
    const done = (outcome: { files: number; differed: boolean; had_index: boolean; backup?: string | null }) =>
      amprIndexStatus("folder", { phase: "done", outcome, dest: "/g/ampr_emu.index" }, false, tr);
    expect(done({ files: 12, differed: true, had_index: true, backup: "/g/ampr_emu.index.bak" })).toBe(
      "Rebuilt: 12 files. The old index was different. The old one is kept as ampr_emu.index.bak.",
    );
    expect(done({ files: 12, differed: false, had_index: true, backup: null })).toBe(
      "Rebuilt: 12 files. The old index was already right.",
    );
    expect(done({ files: 4, differed: true, had_index: false })).toBe(
      "Rebuilt: 4 files. The game had no index before.",
    );
    expect(amprIndexStatus("image", { phase: "done", dest: "/g/G-edited.exfat" }, false, tr)).toBe(
      "Written to /g/G-edited.exfat, with the new index. The image you opened is unchanged.",
    );
    expect(amprIndexStatus("folder", { phase: "failed", message: "no room" }, false, tr)).toBe("no room");
  });

  it("is off with its reason for a game without AMPR, and while a build runs", () => {
    const render = (source: string, kind: "ok" | "no_ampr", locked = false) =>
      renderToStaticMarkup(
        <AmprIndexTool
          source={source}
          inspection={inspection(kind)}
          locked={locked}
          onConvertAgain={() => {}}
          onRebuilt={() => {}}
        />,
      );
    const button = (html: string) => html.slice(html.indexOf("<button"), html.indexOf("Rebuild AMPR index"));
    const off = render("/games/G", "no_ampr");
    expect(button(off)).toContain('disabled=""');
    expect(off).toContain("This one does not.");
    expect(button(render("/games/G", "ok"))).not.toContain('disabled=""');
    expect(button(render("/games/G", "ok", true))).toContain('disabled=""');
    expect(render("/games/G.pkg", "ok")).toBe("");
  });
});
