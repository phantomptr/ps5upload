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
vi.mock("../../lib/tauriEnv", () => ({ isTauriEnv: () => false }));

import type { FpkgInspection } from "../../api/fpkg";
import { CompressionTiles } from "./CompressionTiles";
import { firmwareLine, GameCard, type GameCardProps } from "./GameCard";
import { MemoryRouter } from "react-router";
import { OptionsCard } from "./OptionsCard";
import { pendingJournals, SwapJournalsView } from "./SwapJournals";
import type { ConvertBuild, ConvertItem } from "../../state/convertQueue";
import { QueueCard } from "./QueueCard";

const estimates = {
  fast: { bytes: 117e9, seconds: 900 },
  balanced: { bytes: 110e9, seconds: 3600 },
  smallest: { bytes: 109e9, seconds: 5400 },
};

describe("CompressionTiles", () => {
  it("is a radio group with the chosen level checked", () => {
    const out = renderToStaticMarkup(
      <CompressionTiles value="balanced" onChange={() => {}} estimates={estimates} />,
    );
    expect(out).toContain('role="radiogroup"');
    expect((out.match(/role="radio"/g) ?? []).length).toBe(3);
    expect(out).toMatch(/aria-checked="true"[^>]*>[\s\S]*?Balanced/);
    expect(out).toContain("Recommended");
  });

  it("shows each level's size and time for this game, or that it is estimating", () => {
    const out = renderToStaticMarkup(
      <CompressionTiles value="fast" onChange={() => {}} estimates={estimates} />,
    );
    expect(out).toContain("~109 GiB");
    expect(out).toContain("~1 h 0 min");
    const pending = renderToStaticMarkup(
      <CompressionTiles value="fast" onChange={() => {}} estimates="pending" />,
    );
    expect(pending).toContain("Estimating");
  });

  it("shows a dash, not a spinner that never ends, with no game or a failed estimate", () => {
    for (const e of [null, undefined]) {
      const out = renderToStaticMarkup(<CompressionTiles value="fast" onChange={() => {}} estimates={e} />);
      expect(out).not.toContain("Estimating");
      expect(out).toContain("—");
    }
  });
});

describe("OptionsCard free space", () => {
  const props = {
    outputDir: "/out",
    onChangeOutput: () => {},
    onOutputTyped: () => {},
    canBrowse: true,
    compression: "balanced" as const,
    onCompression: () => {},
    estimates: undefined,
    locked: false,
  };

  it("offers every PlayGo language, by its own name", () => {
    const out = renderToStaticMarkup(<OptionsCard {...props} language="de-DE" onLanguage={() => {}} />);
    expect(out).toContain("Game language");
    expect(out).toMatch(/<option value="">[^<]*As the game ships/);
    expect(out).toMatch(/<option value="de-DE" selected="">Deutsch/);
    expect((out.match(/<option value="[a-z]{2}-/g) ?? []).length).toBe(31);
  });

  it("warns before the build when the output drive is short of room", () => {
    const low = renderToStaticMarkup(<OptionsCard {...props} plannedSize={100 * 2 ** 30} outputFree={50 * 2 ** 30} />);
    expect(low).toContain("Low on free space");
    const fine = renderToStaticMarkup(<OptionsCard {...props} plannedSize={100 * 2 ** 30} outputFree={500 * 2 ** 30} />);
    expect(fine).not.toContain("Low on free space");
    expect(fine).toContain("500 GiB");
  });
});

describe("GameCard firmware line", () => {
  const base = { source: "s", files: 1, bytes: 1, planned_size: 1, checks: [] } as FpkgInspection;
  const tr = (_k: string, vars?: Record<string, string | number>, fallback?: string) => {
    let s = fallback ?? "";
    for (const [k, v] of Object.entries(vars ?? {})) s = s.replace(`{${k}}`, String(v));
    return s;
  };

  it("names the backport's firmware when the modules allow lower", () => {
    expect(firmwareLine({ ...base, required_firmware: "10.20", min_firmware: "4.00" }, tr)).toBe(
      "Runs on FW 4.00 and later (backported)",
    );
  });

  it("falls back to the declared firmware, and says nothing when there is none", () => {
    expect(firmwareLine({ ...base, required_firmware: "05.10", min_firmware: null }, tr)).toBe(
      "Runs on FW 5.10 and later",
    );
    expect(firmwareLine(base, tr)).toBeNull();
  });
});

describe("GameCard archive source", () => {
  const props = (source: string): GameCardProps => ({
    source,
    onSourceTyped: () => {},
    onCheck: () => {},
    onBrowseFolder: () => {},
    onBrowseImage: () => {},
    onRemotePick: () => {},
    canBrowse: true,
    inspection: null,
    checking: false,
    locked: false,
    dropActive: false,
    password: "",
    onPassword: () => {},
  });

  it("says an archive is unpacked when the run starts, and asks a .rar for its password", () => {
    const rar = renderToStaticMarkup(
      <MemoryRouter>
        <GameCard {...props("/dl/game.part1.rar")} />
      </MemoryRouter>,
    );
    expect(rar).toContain("unpacked into the output folder");
    expect(rar).toContain('type="password"');
    const zip = renderToStaticMarkup(
      <MemoryRouter>
        <GameCard {...props("/dl/game.zip")} />
      </MemoryRouter>,
    );
    expect(zip).toContain("unpacked into the output folder");
    expect(zip).not.toContain('type="password"');
    const folder = renderToStaticMarkup(
      <MemoryRouter>
        <GameCard {...props("/games/g")} />
      </MemoryRouter>,
    );
    expect(folder).not.toContain("unpacked");
  });
});

describe("unfinished swaps", () => {
  const j = (step: "installing" | "installed") => ({
    v: 1 as const,
    titleId: "PPSA30528",
    dump: "/data/homebrew/G.exfat",
    parked: "/data/ps5upload/parked/G.exfat",
    packagePath: "/out/a.pkg",
    step,
    at: 0,
  });

  it("offers to put back a dump whose install never finished", () => {
    const out = renderToStaticMarkup(
      <SwapJournalsView journals={[j("installing")]} busy={null} onRollback={() => {}} onFinish={() => {}} />,
    );
    expect(out).toContain("/data/ps5upload/parked/G.exfat");
    expect(out).toContain("Put the dump back");
    expect(out).not.toContain("Delete the old dump");
  });

  it("offers delete or keep once the package installed", () => {
    const out = renderToStaticMarkup(
      <SwapJournalsView journals={[j("installed")]} busy={null} onRollback={() => {}} onFinish={() => {}} />,
    );
    expect(out).toContain("Delete the old dump");
    expect(out).toContain("Keep it parked");
    expect(out).not.toContain("Put the dump back");
  });

  it("leaves out the swap the result card already shows", () => {
    expect(pendingJournals([j("installed")], "PPSA30528")).toEqual([]);
    expect(pendingJournals([j("installed")], null)).toHaveLength(1);
  });
});

describe("QueueCard adding", () => {
  const noop = () => {};
  const card = (
    onPickSeveral?: () => void,
    build: ConvertBuild = { kind: "pkg" },
    items: ConvertItem[] = [],
  ) =>
    renderToStaticMarkup(
      <QueueCard
        items={items}
        build={build}
        onBuild={noop}
        running={false}
        then="keep"
        onThen={noop}
        deleteAfter={false}
        onDeleteAfter={noop}
        canInstall={false}
        canAddCurrent={false}
        onAddCurrent={noop}
        onScanFolder={noop}
        onPickSeveral={onPickSeveral}
        onStart={noop}
        onStop={noop}
        onRemove={noop}
        onMove={noop}
        onClearFinished={noop}
      />,
    );

  it("offers picking several images or archives at once", () => {
    expect(card(noop)).toContain("Pick several…");
    expect(card()).not.toContain("Pick several…");
  });
  it("says which file each queued game becomes", () => {
    const item = (id: string, build?: ConvertBuild): ConvertItem => ({
      id,
      source: `/games/${id}`,
      compression: "balanced",
      then: "keep",
      host: null,
      status: "pending",
      build,
    });
    const html = card(undefined, { kind: "pkg" }, [
      item("old"),
      item("small", { kind: "image", format: "ffpfs", compress: true }),
      item("plain", { kind: "image", format: "exfat", compress: false }),
    ]);
    expect(html).toContain(".pkg");
    expect(html).toContain(".ffpfsc");
    expect(html).toContain(".exfat");
  });

  it("making images offers keeping or uploading the image, not installing a package", () => {
    const html = card(undefined, { kind: "image", format: "ffpfs", compress: true });
    expect(html).toContain("A game image (.ffpfsc)");
    expect(html).toContain("Keep the image");
    expect(html).not.toContain("Stream &amp; install");
  });
});
