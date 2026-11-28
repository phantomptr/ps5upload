import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import type { ArchiveRow } from "../api/archiveEntries";
import { ArchiveChooser, ArchiveRowList, ChoiceSummary } from "./ArchiveChooser";
import { choiceOf } from "../lib/archiveChoice";
import { ArchiveChooseCard } from "../screens/Upload/ArchiveChooseCard";
import { packageRows, packageTicks } from "../screens/InstallPackage/ArchivePackagesCard";

const rows: ArchiveRow[] = [
  {
    path: "Game/base.pkg",
    kind: "package",
    size: 2048,
    files: 1,
    package: { title: "Astro", title_id: "PPSA01234", content_id: "", category: "gd", app_ver: "01.000" },
  },
  {
    path: "Game/patch.pkg",
    kind: "package",
    size: 1024,
    files: 1,
    package: { title: "", title_id: "PPSA01234", content_id: "", category: "gp", app_ver: "01.200" },
  },
  { path: "Extras", kind: "folder", size: 10, files: 3 },
  { path: "readme.txt", kind: "file", size: 5, files: 1 },
];

describe("archive chooser (#969)", () => {
  it("lists each row with its tick, what a package is, and a folder's file count", () => {
    const html = renderToStaticMarkup(
      <ArchiveRowList rows={rows} ticked={new Set(["Game/base.pkg", "Extras"])} onToggle={() => {}} />,
    );
    expect(html).toContain("Game/base.pkg");
    expect(html).toContain("Astro · PPSA01234 · Game · v01.000");
    expect(html).toContain("PPSA01234 · Update · v01.200");
    expect(html).toContain("Folder, 3 file(s)");
    expect(html).toContain("readme.txt");
    expect((html.match(/type="checkbox"/g) ?? []).length).toBe(4);
    expect((html.match(/checked=""/g) ?? []).length).toBe(2);
  });

  it("says how much is chosen and offers select all and none", () => {
    const html = renderToStaticMarkup(
      <ChoiceSummary
        choice={choiceOf(rows, new Set(["Game/patch.pkg", "readme.txt"]))}
        total={rows.length}
        onAll={() => {}}
        onNone={() => {}}
      />,
    );
    expect(html).toContain("2 of 4 chosen");
    expect(html).toContain("Select all");
    expect(html).toContain("Select none");
  });

  it("reads the archive when it opens, and Use these waits for the listing", () => {
    const load = vi.fn(() => new Promise<never>(() => {}));
    const html = renderToStaticMarkup(
      <ArchiveChooser open archivePath="/x/Bundle.zip" onCancel={() => {}} onConfirm={() => {}} load={load} />,
    );
    expect(html).toContain("Choose files in Bundle.zip");
    expect(html).toContain("Reading the archive");
  });

  it("the upload card says whether everything or some of the archive goes", () => {
    const all = renderToStaticMarkup(<ArchiveChooseCard archivePath="/x/a.zip" chosen={null} onChoose={() => {}} />);
    expect(all).toContain("Everything in the archive is sent.");
    expect(all).toContain("Choose files…");
    const some = renderToStaticMarkup(
      <ArchiveChooseCard archivePath="/x/a.zip" chosen={["Extras", "readme.txt"]} chosenBytes={2048} onChoose={() => {}} compact />,
    );
    expect(some).toContain("Only 2 chosen item(s) are sent (2.00 KiB).");
  });

  it("the install card ticks every package to start with, or the chosen ones", () => {
    const pkgs = [
      { path: "a.pkg", size: 1, info: rows[0].package },
      { path: "b.pkg", size: 2 },
    ];
    expect(packageRows(pkgs).map((r) => [r.path, r.kind, r.package?.category ?? null])).toEqual([
      ["a.pkg", "package", "gd"],
      ["b.pkg", "package", null],
    ]);
    expect([...packageTicks(pkgs, null)]).toEqual(["a.pkg", "b.pkg"]);
    expect([...packageTicks(pkgs, ["b.pkg"])]).toEqual(["b.pkg"]);
  });
});
