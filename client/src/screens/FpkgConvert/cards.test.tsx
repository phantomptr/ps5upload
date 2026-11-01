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
import { MakeCard } from "./MakeCard";
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
    expect((out.match(/role="radio"/g) ?? []).length).toBe(4);
    expect(out).toContain("Stored");
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

describe("MakeCard", () => {
  it("explains both, and marks the image for a PS5 game the firmware will not play as a package", () => {
    const fw = renderToStaticMarkup(
      <MakeCard make="pkg" onMake={() => {}} kernel="r229358/releases/13.60" contentId="UP9000-PPSA03671_00-X" checked />,
    );
    expect(fw).toContain("A package (.pkg)");
    expect(fw).toContain("A game image");
    expect(fw).toContain("Recommended on FW 13.60");
    expect(fw).toContain("not play on FW 13.60");
    const ps4 = renderToStaticMarkup(
      <MakeCard make="pkg" onMake={() => {}} kernel="r229358/releases/13.60" contentId="UP9000-CUSA01234_00-X" checked />,
    );
    expect(ps4).not.toContain("Recommended on FW");
    expect(ps4.match(/Recommended/g)).toHaveLength(1);
    const none = renderToStaticMarkup(<MakeCard make="pkg" onMake={() => {}} kernel={null} contentId={null} checked={false} />);
    expect(none).not.toContain("Recommended");
    expect(none).toContain("Check a game under");
  });

  it("offers a game folder as a third choice, never marked as recommended", () => {
    const out = renderToStaticMarkup(
      <MakeCard make="folder" onMake={() => {}} kernel="r229358/releases/9.60" contentId="UP9000-PPSA03671_00-X" checked />,
    );
    expect((out.match(/role="radio"/g) ?? []).length).toBe(3);
    expect(out).toContain("A game folder");
    expect(out).toContain("from a game image or a package made with the debug key");
    expect(out).toMatch(/aria-checked="true"[^>]*>[\s\S]*?A game folder/);
    // The one badge is on the package, not the folder.
    expect(out.match(/Recommended/g)).toHaveLength(1);
    expect(out).not.toMatch(/A game folder<\/span><span[^>]*>Recommended/);
  });
});

describe("OptionsCard for a folder", () => {
  it("has no compression, image or language choice, only the folder line and verification", () => {
    const out = renderToStaticMarkup(
      <OptionsCard
        outputDir="/out"
        onChangeOutput={() => {}}
        onOutputTyped={() => {}}
        canBrowse
        compression="balanced"
        onCompression={() => {}}
        estimates={undefined}
        locked={false}
        make="folder"
        language=""
        onLanguage={() => {}}
      />,
    );
    expect(out).toContain("options-folder");
    expect(out).not.toContain("fpkg-language");
    expect(out).not.toContain("Balanced");
    expect(out).not.toContain("options-image");
    expect(out).toContain("Full verification");
  });
});

describe("OptionsCard output place", () => {
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
  it("offers Save each next to its source, explained only when on", () => {
    const off = renderToStaticMarkup(<OptionsCard {...props} outputPlace="folder" onOutputPlace={() => {}} />);
    expect(off).toContain("Save each next to its source");
    expect(off).not.toContain("output-beside-hint");
    const on = renderToStaticMarkup(<OptionsCard {...props} outputPlace="beside" onOutputPlace={() => {}} />);
    expect(on).toMatch(/data-testid="output-beside"[^>]*checked/);
    expect(on).toContain("output-beside-hint");
    expect(on).toContain("(2) is added");
  });
  it("hides the choice where it is not wired", () => {
    expect(renderToStaticMarkup(<OptionsCard {...props} />)).not.toContain("output-beside");
  });
});

describe("OptionsCard for an image", () => {
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
  it("shows the image's options in place of the package's compression and language", () => {
    const out = renderToStaticMarkup(
      <OptionsCard
        {...props}
        make="image"
        image={{ format: "ffpkg", compress: false, onChoice: () => {} }}
        language="de-DE"
        onLanguage={() => {}}
      />,
    );
    expect(out).toContain(".ffpkg (UFS2), recommended");
    expect(out).toContain("Compress it into a .ffpfsc");
    expect(out).not.toContain("fpkg-language");
    expect(out).not.toContain("Balanced");
    const pkg = renderToStaticMarkup(<OptionsCard {...props} make="pkg" language="" onLanguage={() => {}} />);
    expect(pkg).toContain("fpkg-language");
  });

  it("offers the .ffpfsc levels only when compressing", () => {
    const render = (compress: boolean) =>
      renderToStaticMarkup(
        <OptionsCard
          {...props}
          make="image"
          image={{ format: "ffpkg", compress, onChoice: () => {} }}
          language=""
          onLanguage={() => {}}
        />,
      );
    expect(render(false)).not.toContain("ffpfsc-level");
    const on = render(true);
    expect(on).toContain("ffpfsc-level");
    for (const label of ["Fast", "Balanced", "Smallest", "Recommended"]) expect(on).toContain(label);
    // Balanced is the default.
    expect(on).toMatch(/aria-checked="true"[^>]*>(<[^>]+>)*Balanced/);
  });
});

describe("OptionsCard verification", () => {
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
  it("offers full verification, off by default, for a package and for an image", () => {
    for (const make of ["pkg", "image"] as const) {
      const out = renderToStaticMarkup(<OptionsCard {...props} make={make} />);
      expect(out).toContain("Full verification");
      expect(out).toMatch(/role="switch"[^>]*aria-checked="false"/);
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

  it("does not call an older SDK than declared a backport", () => {
    expect(firmwareLine({ ...base, required_firmware: "10.20", min_firmware: "9.00" }, tr)).toBe(
      "Runs on FW 9.00 and later (its modules allow it; it declares 10.20)",
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
    onAdd: () => {},
    onScanFolder: () => {},
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

  it("is the one place games are added, with the same buttons as Upload & Install", () => {
    const html = renderToStaticMarkup(
      <MemoryRouter>
        <GameCard {...props("")} />
      </MemoryRouter>,
    );
    expect(html).toContain("Choose file");
    expect(html).toContain("Choose folder");
    expect(html).toContain("Add games from a folder…");
    expect(html).toContain("Several games, or a folder of games, go into the queue");
    const locked = renderToStaticMarkup(
      <MemoryRouter>
        <GameCard {...props("/games/g")} locked />
      </MemoryRouter>,
    );
    expect(locked).toContain("wait in the queue until you start it");
  });

  // #621: while a game is being made, another can still be picked (here, or on the PS5) and
  // queued; nothing in ① looks switched off.
  it("lets games be added while one is being made", () => {
    const html = renderToStaticMarkup(
      <MemoryRouter>
        <GameCard {...props("/games/g")} locked queueStarts onBrowseConsole={() => {}} />
      </MemoryRouter>,
    );
    expect(html).toContain("go into the queue and are made after it");
    const at = html.indexOf("On the PS5…");
    expect(at).toBeGreaterThan(0);
    const ps5 = html.slice(html.lastIndexOf("<button", at), at);
    expect(ps5).not.toContain("disabled=\"\"");
    expect(html).not.toContain("opacity-60");
    // The game being made stays put.
    expect(html).toMatch(/id="fpkg-source"[^>]*disabled|disabled[^>]*id="fpkg-source"/);
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
  const card = (items: ConvertItem[] = []) =>
    renderToStaticMarkup(
      <QueueCard
        items={items}
        running={false}
        onOpenUpload={noop}
        onStart={noop}
        onStop={noop}
        onRemove={noop}
        onUpdate={noop}
        canInstall
        host="192.168.1.20"
        onMove={noop}
        onClearFinished={noop}
      />,
    );

  const waiting = (status: ConvertItem["status"] = "pending"): ConvertItem => ({
    id: "w",
    source: "/games/w",
    compression: "balanced",
    then: "stream",
    host: "192.168.1.20",
    status,
  });

  it("is not shown while nothing is queued: games are added under ①", () => {
    expect(card()).toBe("");
  });

  it("has no add buttons and no second set of choices of its own", () => {
    const html = card([waiting()]);
    expect(html).not.toContain("Add games from a folder…");
    expect(html).not.toContain("Choose file");
    expect(html).not.toContain("<select");
    expect(html).toContain("⑤ Queue");
    expect(html).toContain("Add more under ①");
  });

  it("offers Upload & Install once a made game has been handed to its queue", () => {
    expect(card([waiting()])).not.toContain("Open Upload &amp; Install");
    expect(card([waiting("handed")])).toContain("Open Upload &amp; Install");
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
    const html = card([
      item("old"),
      item("small", { kind: "image", format: "ffpfs", compress: true }),
      item("plain", { kind: "image", format: "exfat", compress: false }),
      item("back", { kind: "folder" }),
    ]);
    expect(html).toContain(".pkg");
    expect(html).toContain(".ffpfsc");
    expect(html).toContain(".exfat");
    expect(html).toMatch(/data-testid="cq-ext"[^>]*>folder</);
  });
});

describe("queued game choices (#518)", () => {
  it("offers what each source can become", async () => {
    const { kindsFor } = await import("./QueueItemOptions");
    expect(kindsFor("/games/Astro Bot")).toEqual(["pkg", "image"]);
    expect(kindsFor("/dl/game.exfat")).toEqual(["pkg", "image", "folder"]);
    expect(kindsFor("/dl/game.zip")).toEqual(["pkg", "image"]);
    expect(kindsFor("/dl/notes.txt")).toEqual(["pkg"]);
  });

  it("keeps an image's format and moves the after-step to what the new kind can do", async () => {
    const { changeForKind } = await import("./QueueItemOptions");
    const item = {
      id: "c1",
      source: "/games/x",
      compression: "zlib",
      build: { kind: "pkg" },
      then: "stream",
      host: "10.0.0.2",
      status: "pending",
    } as unknown as import("../../state/convertQueue").ConvertItem;
    expect(changeForKind(item, "image", true, null)).toEqual({
      build: { kind: "image", format: "exfat", compress: false },
      then: "upload",
      host: "10.0.0.2",
    });
    expect(changeForKind(item, "folder", true, null)).toEqual({ build: { kind: "folder" }, then: "keep", host: null });
  });
});

describe("convert queue update", () => {
  it("changes a waiting game and leaves a running one alone", async () => {
    const { useConvertQueue } = await import("../../state/convertQueue");
    const q = useConvertQueue.getState();
    q.add({ source: "/games/a", compression: "zlib" as never, build: { kind: "pkg" }, then: "keep", host: null });
    const all = useConvertQueue.getState().items;
    const id = all[all.length - 1].id;
    expect(useConvertQueue.getState().update(id, { then: "stream", host: "h" })).toBe(true);
    expect(useConvertQueue.getState().items.find((i) => i.id === id)?.then).toBe("stream");
    useConvertQueue.setState({ items: useConvertQueue.getState().items.map((i) => (i.id === id ? { ...i, status: "running" } : i)) });
    expect(useConvertQueue.getState().update(id, { then: "keep" })).toBe(false);
  });
});
