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
  it("says the level only trades build time for size, and Stored is the odd one out", () => {
    const html = renderToStaticMarkup(<CompressionTiles value="balanced" onChange={() => {}} estimates={null} />);
    expect(html).toContain("Same game either way");
    const stored = renderToStaticMarkup(<CompressionTiles value="stored" onChange={() => {}} estimates={null} />);
    expect(stored).not.toContain("Same game either way");
  });

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
  it("does not ask to check an archive, which is looked at when it is unpacked", () => {
    const html = renderToStaticMarkup(
      <MakeCard make="pkg" onMake={() => {}} checked={false} archive />,
    );
    expect(html).not.toContain("Check a game");
    expect(html).toContain("after it is unpacked");
  });

  it("explains both, and marks the package once a game is checked", () => {
    const fw = renderToStaticMarkup(<MakeCard make="pkg" onMake={() => {}} checked />);
    expect(fw).toContain("A fake package (FPKG, .pkg)");
    // It says in words what a .pkg is, and the two ways to play it: install, or Kraken Mount.
    expect(fw).toContain("A fake package (FPKG). There are two ways to play a PS5 one: install it");
    expect(fw).toContain("or mount it with Kraken Mount");
    expect(fw).toContain("A game image");
    expect(fw.match(/Recommended/g)).toHaveLength(1);
    expect(fw).toContain("needs the latest kstuff (with fake-package support up to 13.60)");
    const ps4 = renderToStaticMarkup(
      <MakeCard make="pkg" onMake={() => {}} checked />,
    );
    expect(ps4).not.toContain("Recommended on FW");
    expect(ps4.match(/Recommended/g)).toHaveLength(1);
    const none = renderToStaticMarkup(<MakeCard make="pkg" onMake={() => {}} checked={false} />);
    expect(none).not.toContain("Recommended");
    expect(none).toContain("Check a game under");
  });

  it("with Kraken Mount running, marks the package (mounted, not installed) and says images aren't mounted", () => {
    const k = renderToStaticMarkup(
      <MakeCard make="pkg" onMake={() => {}} checked kraken />,
    );
    expect(k).not.toContain("Recommended on FW");
    expect(k.match(/Recommended/g)).toHaveLength(1);
    expect(k).toMatch(/A fake package \(FPKG, \.pkg\)[\s\S]*?Recommended/);
    expect(k).toContain("a package made here is mounted from a drive, not installed");
    expect(k).toContain("Kraken Mount, running on this PS5, mounts it");
    const img = renderToStaticMarkup(
      <MakeCard make="image" onMake={() => {}} checked={false} kraken />,
    );
    expect(img).toContain("does not mount images");
  });

  it("offers a game folder as a third choice, never marked as recommended", () => {
    const out = renderToStaticMarkup(
      <MakeCard make="folder" onMake={() => {}} checked />,
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

describe("A PS4 game's package", () => {
  it("is offered as a PS4 package under ②", () => {
    const out = renderToStaticMarkup(
      <MakeCard make="pkg" onMake={() => {}} checked ps4 />,
    );
    expect(out).toContain("A PS4 fake package (FPKG, .pkg)");
    expect(out).toContain("A PS4 fake package the PS5 installs");
    expect(out).not.toContain("A fake package (FPKG, .pkg)");
    expect((out.match(/role="radio"/g) ?? []).length).toBe(3);
  });

  it("has no PS5 package's compression or language under ③, and says it is read back whole", () => {
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
        make="pkg"
        language=""
        onLanguage={() => {}}
        ps4
      />,
    );
    expect(out).toContain("options-ps4-pkg");
    expect(out).toContain("every file is read back and compared");
    expect(out).not.toContain("fpkg-language");
    expect(out).not.toContain("Balanced");
    expect(out).not.toContain("Full verification");
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
    // The package's compression tiles are a radiogroup (the "Which image" help names the
    // .ffpfsc levels in prose, so look for the control, not the word).
    expect(out).not.toContain('role="radiogroup"');
    const pkg = renderToStaticMarkup(<OptionsCard {...props} make="pkg" language="" onLanguage={() => {}} />);
    expect(pkg).toContain("fpkg-language");
  });

  it("offers the game's own languages first, then every language, then each one (#939)", () => {
    const render = (language: string) =>
      renderToStaticMarkup(<OptionsCard {...props} make="pkg" language={language} onLanguage={() => {}} />);
    const own = render("");
    expect(own).toMatch(/<option value="" selected="">The game&#x27;s own languages<\/option>/);
    expect(own).toContain('<option value="all">Every language</option>');
    // 31 languages after the two.
    expect((own.match(/<option /g) ?? []).length).toBe(33);
    // Every language is the one choice that can fill the console's list: it says so.
    expect(own).not.toContain("language-every-hint");
    expect(render("fr-FR")).not.toContain("language-every-hint");
    const every = render("all");
    expect(every).toContain("language-every-hint");
    expect(every).toContain("all 31 languages");
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

describe("OptionsCard for a package of a game in asset packs (#933)", () => {
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
  const pkgUnpack = (on: boolean, leaveOut = false) => ({
    kind: "zstd" as const,
    on,
    onToggle: () => {},
    leaveOutEmulator: { on: leaveOut, onToggle: () => {} },
  });
  it("offers the plain files beside the compression, and leaving the emulator out only under them", () => {
    const on = renderToStaticMarkup(<OptionsCard {...props} make="pkg" unpack={pkgUnpack(true)} />);
    expect(on).toMatch(/data-testid="unpack-option"[^>]*>\s*<input[^>]*checked/);
    expect(on).toContain("the package holds every file as a plain file");
    expect(on).toContain('role="radiogroup"');
    expect(on).toContain('data-testid="leave-out-ampr-emu"');
    expect(on).toContain("Leave out the AMPR emulator");
    expect(on).not.toMatch(/data-testid="leave-out-ampr-emu"[^>]*>\s*<input[^>]*checked/);
    const left = renderToStaticMarkup(<OptionsCard {...props} make="pkg" unpack={pkgUnpack(true, true)} />);
    expect(left).toMatch(/data-testid="leave-out-ampr-emu"[^>]*>\s*<input[^>]*checked/);
    // Unticked, the packs go in as they are and the emulator has to stay.
    const off = renderToStaticMarkup(<OptionsCard {...props} make="pkg" unpack={pkgUnpack(false, true)} />);
    expect(off).toContain('data-testid="unpack-option"');
    expect(off).not.toMatch(/data-testid="unpack-option"[^>]*>\s*<input[^>]*checked/);
    expect(off).not.toContain("leave-out-ampr-emu");
  });
  it("offers no emulator choice for an image, and nothing for a PS4 package", () => {
    const image = renderToStaticMarkup(
      <OptionsCard
        {...props}
        make="image"
        image={{ format: "ffpkg", compress: false, onChoice: () => {} }}
        unpack={{ kind: "lz4", on: true, onToggle: () => {} }}
      />,
    );
    expect(image).toContain('data-testid="unpack-option"');
    expect(image).toContain("no longer needs ShadowMount+ to read packs");
    expect(image).not.toContain("leave-out-ampr-emu");
    const ps4 = renderToStaticMarkup(<OptionsCard {...props} make="pkg" ps4 unpack={pkgUnpack(true)} />);
    expect(ps4).not.toContain("unpack-option");
  });

  // Someone looked for "create fpkg without ampr" and found nothing: the two options only
  // showed for a checked game in packs, under the output folder and the compression tiles.
  it("puts them first under ③, in a group named for asset packs and AMPR", () => {
    const html = renderToStaticMarkup(<OptionsCard {...props} make="pkg" gameChecked unpack={pkgUnpack(true)} />);
    expect(html).toMatch(/role="group" aria-label="Asset packs and AMPR"[^>]*data-testid="pkg-packs"/);
    const at = (needle: string) => html.indexOf(needle);
    expect(at("pkg-packs")).toBeGreaterThan(0);
    expect(at("pkg-packs")).toBeLessThan(at("Output folder"));
    expect(at("unpack-option")).toBeLessThan(at('role="radiogroup"'));
    expect(at("leave-out-ampr-emu")).toBeLessThan(at("Output folder"));
    // The note is for a game with nothing to choose.
    expect(html).not.toContain("pkg-packs-note");
  });

  it("says where the options are before a game is checked, and why there are none for a plain game", () => {
    const pending = renderToStaticMarkup(<OptionsCard {...props} make="pkg" />);
    expect(pending).toMatch(/data-testid="pkg-packs-note" data-state="pending"/);
    expect(pending).toContain("Asset packs and AMPR");
    expect(pending).toContain(
      "Check a game under ① first. If it is in asset packs, the options to unpack them and to leave out the AMPR emulator appear here.",
    );
    expect(pending).not.toContain("unpack-option");
    const plain = renderToStaticMarkup(<OptionsCard {...props} make="pkg" gameChecked />);
    expect(plain).toMatch(/data-testid="pkg-packs-note" data-state="none"/);
    expect(plain).toContain("This game has no asset packs, so there is nothing to unpack.");
    expect(plain).not.toContain("Check a game under ① first");
    // One line each, in the same place the options take.
    for (const html of [pending, plain]) expect(html.indexOf("pkg-packs-note")).toBeLessThan(html.indexOf("Output folder"));
  });

  it("says nothing of a package's packs for an image, a folder or a PS4 package", () => {
    const image = renderToStaticMarkup(
      <OptionsCard {...props} make="image" image={{ format: "ffpkg", compress: false, onChoice: () => {} }} />,
    );
    const folder = renderToStaticMarkup(<OptionsCard {...props} make="folder" />);
    const ps4 = renderToStaticMarkup(<OptionsCard {...props} make="pkg" ps4 gameChecked />);
    for (const html of [image, folder, ps4]) {
      expect(html).not.toContain("pkg-packs");
      expect(html).not.toContain("Asset packs and AMPR");
    }
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
    expect(out).toMatch(/<option value="">[^<]*The game&#x27;s own languages/);
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
    // A .zip, .7z or .rar is read in place for an image or a folder.
    expect(zip).toContain("with nothing unpacked");
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

  // The check under ① is of the source as it is. A packed game's own ampr_emu.index goes into
  // what keeps the packs, and a fresh one into anything else: the finding is a warning only for
  // the first (a .ffpkg in zstd packs made into a .pkg warned although the package was fine).
  it("warns about the game's stale index only when what is made carries it", () => {
    const stale = "the folder's ampr_emu.index is stale (7 listed missing, 0 unlisted, 10 sizes differ)";
    const inspection = (applies_to?: string): FpkgInspection =>
      ({
        source: "image /games/Packed.ffpkg",
        files: 3,
        bytes: 3 << 20,
        content_id: "UP9000-PPSA01234_00-PACKEDGAME000000",
        title: "Packed Game",
        planned_size: 3 << 20,
        output_free: 1 << 40,
        packs: "zstd",
        checks: [
          { name: "eboot.bin present", ok: true, detail: "the title module" },
          { name: "ampr_emu.index for a libSceAmpr title", ok: false, detail: stale, applies_to },
        ],
      }) as FpkgInspection;
    const render = (applies_to: string | undefined, packsKept: boolean) =>
      renderToStaticMarkup(
        <MemoryRouter>
          <GameCard {...props("/games/Packed.ffpkg")} inspection={inspection(applies_to)} packsKept={packsKept} />
        </MemoryRouter>,
      );
    const warned = (html: string) => html.includes("text-[var(--color-warn)]") && html.includes("⚠");
    // The packs stay: the stale index ships, and it is a warning.
    const kept = render("packs_kept", true);
    expect(warned(kept)).toBe(true);
    expect(kept).not.toContain("check-replaced");
    // Unpacked (or packed again): a fresh index is written. No warning; the finding is still
    // there to read with the other checks.
    const unpacked = render("packs_kept", false);
    expect(warned(unpacked)).toBe(false);
    expect(unpacked).toContain("✓");
    expect(unpacked).toContain("Checks passed: 1");
    expect(unpacked).toMatch(/data-testid="check-replaced"[^>]*>[^<]*is stale \(7 listed missing/);
    // A game in no packs gets a fresh index in everything made from it.
    expect(warned(render("source_as_is", false))).toBe(false);
    // A finding with no scope (an older engine, or any other check) warns as before.
    expect(warned(render(undefined, false))).toBe(true);
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

  it("says what closing the page does while the queue runs, and that it waits after one (#954)", () => {
    const props = {
      items: [waiting()],
      onOpenUpload: noop,
      onStart: noop,
      onStop: noop,
      onRemove: noop,
      onUpdate: noop,
      canInstall: true,
      host: "192.168.1.20",
      onMove: noop,
      onClearFinished: noop,
    };
    const runningHtml = renderToStaticMarkup(<QueueCard {...props} running />);
    expect(runningHtml).toContain("Closing this page does not stop the game being made");
    expect(runningHtml).not.toContain("cq-stopped-by-close");
    const stopped = renderToStaticMarkup(<QueueCard {...props} running={false} stoppedByClose />);
    expect(stopped).toContain("The queue stopped when this page was closed");
    expect(stopped).not.toContain("Closing this page does not stop");
    // Nothing left to start: nothing to say.
    expect(renderToStaticMarkup(<QueueCard {...props} items={[waiting("done")]} running={false} stoppedByClose />)).not.toContain(
      "cq-stopped-by-close",
    );
    expect(card([waiting()])).not.toContain("cq-stopped-by-close");
  });

  it("a game with a plain % in its name does not break the card", async () => {
    const { nameOf } = await import("./QueueCard");
    expect(nameOf("/games/100% Orange Juice")).toBe("100% Orange Juice");
    expect(nameOf("smb://nas/games/My%20Game/")).toBe("My Game");
    expect(card([{ ...waiting(), source: "/games/100% Orange Juice" }])).toContain("100% Orange Juice");
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

  it("shows each queued game's packs on its row, so one without zstd is seen before the run (#905)", () => {
    const item = (id: string, build: ConvertBuild): ConvertItem => ({
      id,
      source: `/games/${id}`,
      compression: "balanced",
      then: "keep",
      host: null,
      status: "pending",
      build,
    });
    const html = card([
      item("zstd", { kind: "image", format: "ffpkg", compress: false, lz4: { zstdLevel: 19 } }),
      item("none", { kind: "image", format: "ffpkg", compress: false }),
    ]);
    expect(html).toMatch(/data-testid="cq-tag-zstd"[^>]*>zstd 19</);
    expect(html.match(/cq-tag-zstd/g)).toHaveLength(1);
  });
});

describe("queued game choices (#518)", () => {
  it("offers what each source can become", async () => {
    const { kindsFor } = await import("./QueueItemOptions");
    expect(kindsFor("/games/Astro Bot")).toEqual(["pkg", "image"]);
    expect(kindsFor("/dl/game.exfat")).toEqual(["pkg", "image", "folder"]);
    // A .zip, .7z or .rar becomes a folder too, read in place.
    expect(kindsFor("/dl/game.zip")).toEqual(["pkg", "image", "folder"]);
    expect(kindsFor("/dl/notes.txt")).toEqual(["pkg"]);
    // A folder with a dotted name, known to be a folder when it was added.
    expect(kindsFor("/games/Some.Game.2024", true)).toEqual(["pkg", "image"]);
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

describe("a waiting game's own options (#669)", () => {
  const waitingItem = (over: Partial<ConvertItem> = {}): ConvertItem => ({
    id: "w1",
    source: "/games/Astro Bot",
    compression: "balanced",
    then: "keep",
    host: null,
    status: "pending",
    ...over,
  });
  const options = async (item: ConvertItem) => {
    const { QueueItemOptions } = await import("./QueueItemOptions");
    return renderToStaticMarkup(
      <QueueItemOptions item={item} canInstall host="10.0.0.2" canBrowse onPickOutput={async () => null} onChange={() => {}} />,
    );
  };

  it("an image being uploaded shows where it goes, and one kept or a package does not (#673)", async () => {
    const image: ConvertBuild = { kind: "image", format: "exfat", compress: true };
    const up = await options(waitingItem({ build: image, then: "upload", host: "10.0.0.2", imageDest: { volume: "/data", subpath: "games" } }));
    expect(up).toContain('data-testid="image-dest-drive"');
    expect(up).toContain("Goes to /data/games/…");
    expect(await options(waitingItem({ build: image, then: "keep" }))).not.toContain("image-dest");
    expect(await options(waitingItem({ build: { kind: "pkg" }, then: "upload", host: "10.0.0.2" }))).not.toContain("image-dest");
  });

  it("a package has its output folder, compression, language, verification and what comes after", async () => {
    const html = await options(waitingItem({ build: { kind: "pkg" }, outputDir: "F:\\out" }));
    expect(html).toContain("Output folder");
    expect(html).toContain('value="F:\\out"');
    expect(html).toContain("Change");
    expect(html).toContain("Save each next to its source");
    expect(html).toContain("Compression");
    expect(html).toContain("Game language");
    expect(html).toContain("Full verification");
    expect(html).toContain("Stream &amp; install");
    expect(html).toContain("Upload &amp; install");
  });

  it("an image has its format and compression level instead, and a folder neither", async () => {
    const image = await options(waitingItem({ build: { kind: "image", format: "ffpkg", compress: true } }));
    expect(image).toContain('data-testid="image-choice"');
    expect(image).toContain('data-testid="ffpfsc-level"');
    expect(image).not.toContain("Game language");
    const folder = await options(waitingItem({ source: "/dl/game.exfat", build: { kind: "folder" } }));
    expect(folder).not.toContain('data-testid="image-choice"');
    expect(folder).not.toContain("Game language");
    expect(folder).toContain("Output folder");
    expect(folder).toContain("Keep the folder");
  });

  it("shows a waiting image's or folder's asset packs, and a package has none (#905)", async () => {
    const zstd = await options(
      waitingItem({ build: { kind: "image", format: "ffpkg", compress: true, lz4: { zstdLevel: 19 } } }),
    );
    expect(zstd).toContain('data-testid="cq-packs"');
    expect(zstd).toContain("Asset packs");
    expect(zstd).toMatch(/data-testid="cq-zstd-level"/);
    expect(zstd).toMatch(/<option value="19" selected="">19<\/option>/);
    const plain = await options(waitingItem({ build: { kind: "image", format: "exfat", compress: false } }));
    expect(plain).toContain('data-testid="cq-packs"');
    expect(plain).not.toContain("cq-zstd-level");
    // LZ4 is offered only to keep the choice the game was checked for.
    expect(plain).not.toContain(">LZ4<");
    const folder = await options(waitingItem({ source: "/dl/game.exfat", build: { kind: "folder", zstd: { level: 1 } } }));
    expect(folder).toContain("cq-zstd-level");
    expect(await options(waitingItem({ build: { kind: "pkg" } }))).not.toContain("cq-packs");
  });

  // The same two choices while a package waits in the queue: shown for a game known to be in
  // packs, with one line otherwise.
  it("shows a waiting package's asset-pack choices, or says why it has none", async () => {
    const packed = await options(waitingItem({ build: { kind: "pkg", unpackPacks: true }, packs: "zstd" }));
    expect(packed).toMatch(/aria-label="Asset packs and AMPR"[^>]*data-testid="pkg-packs"/);
    expect(packed).toMatch(/data-testid="unpack-option"[^>]*>\s*<input[^>]*checked/);
    expect(packed).toContain("Unpack the asset packs");
    expect(packed).toContain("Leave out the AMPR emulator");
    expect(packed).not.toMatch(/data-testid="leave-out-ampr-emu"[^>]*>\s*<input[^>]*checked/);
    expect(packed.indexOf("pkg-packs")).toBeLessThan(packed.indexOf("Compression"));
    const left = await options(
      waitingItem({ build: { kind: "pkg", unpackPacks: true, leaveOutAmprEmu: true }, packs: "lz4" }),
    );
    expect(left).toMatch(/data-testid="leave-out-ampr-emu"[^>]*>\s*<input[^>]*checked/);
    // Checked under ① and unticked there: still its choice to make.
    const kept = await options(waitingItem({ build: { kind: "pkg" }, packs: "zstd" }));
    expect(kept).toContain('data-testid="unpack-option"');
    expect(kept).not.toMatch(/data-testid="unpack-option"[^>]*>\s*<input[^>]*checked/);
    expect(kept).not.toContain("leave-out-ampr-emu");
    // Queued with its packs unpacked before the queue remembered what the game is in.
    expect(await options(waitingItem({ build: { kind: "pkg", unpackPacks: true } }))).toContain(
      'data-testid="unpack-option"',
    );

    const plain = await options(waitingItem({ build: { kind: "pkg" }, packs: "none" }));
    expect(plain).toMatch(/data-testid="pkg-packs-note" data-state="none"/);
    expect(plain).toContain("This game has no asset packs, so there is nothing to unpack.");
    const unchecked = await options(waitingItem({ build: { kind: "pkg" } }));
    expect(unchecked).toMatch(/data-testid="pkg-packs-note" data-state="unchecked"/);
    expect(unchecked).toContain("If it is in asset packs, the package gets them unpacked.");
    expect(unchecked).toContain("check the game under ① and add it from there");
    // An image or a folder has its own packs row, not the package's.
    expect(await options(waitingItem({ build: { kind: "image", format: "exfat", compress: false } }))).not.toContain(
      "pkg-packs",
    );
  });

  it("changes a waiting package's asset-pack choices, and keeps what was unticked by hand", async () => {
    const { pkgUnpackChoice } = await import("./QueueItemOptions");
    const changes: unknown[] = [];
    const choice = (item: ConvertItem) => pkgUnpackChoice(item, (c) => changes.push(c))!;
    const packed = waitingItem({ build: { kind: "pkg", unpackPacks: true }, packs: "zstd" });
    choice(packed).leaveOutEmulator!.onToggle(true);
    choice(packed).onToggle(false);
    choice(waitingItem({ build: { kind: "pkg", unpackPacks: true, leaveOutAmprEmu: true }, packs: "zstd" })).onToggle(
      false,
    );
    choice(waitingItem({ build: { kind: "pkg" }, packs: "lz4" })).onToggle(true);
    expect(changes).toEqual([
      { build: { kind: "pkg", unpackPacks: true, leaveOutAmprEmu: true } },
      // Unticked: said outright, so the queue doesn't look again and unpack it after all.
      { build: { kind: "pkg", unpackPacks: false } },
      // Leaving the emulator out goes with unpacking.
      { build: { kind: "pkg", unpackPacks: false } },
      { build: { kind: "pkg", unpackPacks: true } },
    ]);
    // A game nobody checked, or one in no packs, has nothing to choose.
    expect(pkgUnpackChoice(waitingItem({ build: { kind: "pkg" } }), () => {})).toBeUndefined();
    expect(pkgUnpackChoice(waitingItem({ build: { kind: "pkg" }, packs: "none" }), () => {})).toBeUndefined();
  });

  it("changing packs, format or kind keeps the rest of the game's build (#905)", async () => {
    const { changeForPacks, changeForKind } = await import("./QueueItemOptions");
    const packed: ConvertBuild = {
      kind: "image",
      format: "ffpkg",
      compress: true,
      lz4: { zstdLevel: 5, zstdRuntime: "pinned", profileToml: "[p]" },
    };
    expect(changeForPacks(packed, "zstd", 19)).toEqual({
      build: { ...packed, lz4: { zstdLevel: 19, zstdRuntime: "pinned", profileToml: "[p]" } },
    });
    expect(changeForPacks(packed, "none")).toEqual({ build: { kind: "image", format: "ffpkg", compress: true } });
    expect(changeForPacks({ kind: "image", format: "exfat", compress: false }, "zstd")).toEqual({
      build: { kind: "image", format: "exfat", compress: false, lz4: { zstdLevel: 5 } },
    });
    const item = waitingItem({ source: "/dl/game.exfat", build: packed });
    expect(changeForKind(item, "folder", true, null).build).toEqual({
      kind: "folder",
      zstd: { level: 5, runtime: "pinned", profileToml: "[p]" },
    });
    expect(changeForKind(item, "image", true, null).build).toEqual(packed);
    // A folder's own segment picked again keeps its packs and plain-file choice.
    const folder: ConvertBuild = { kind: "folder", zstd: { level: 19, runtime: "pinned" } };
    expect(changeForKind(waitingItem({ build: folder }), "folder", true, null).build).toEqual(folder);
    const unpacked: ConvertBuild = { kind: "folder", unpackPacks: true };
    expect(changeForKind(waitingItem({ build: unpacked }), "folder", true, null).build).toEqual(unpacked);
    // #933: plain files go along into another kind; leaving the emulator out is a package's own.
    const pkg: ConvertBuild = { kind: "pkg", unpackPacks: true, leaveOutAmprEmu: true };
    expect(changeForKind(waitingItem({ source: "/dl/game.exfat", build: pkg }), "folder", true, null).build).toEqual({
      kind: "folder",
      unpackPacks: true,
    });
    expect(changeForKind(waitingItem({ source: "/dl/game.exfat", build: unpacked }), "pkg", true, null).build).toEqual({
      kind: "pkg",
      unpackPacks: true,
    });
    expect(changeForKind(waitingItem({ build: pkg }), "pkg", true, null).build).toEqual(pkg);
  });

  it("a game on a server or the PS5 has no folder here to be saved next to", async () => {
    const html = await options(waitingItem({ source: "ps5://10.0.0.2/data/game.exfat", build: { kind: "pkg" } }));
    expect(html).not.toContain("Save each next to its source");
    expect(html).toContain("Output folder");
  });

  it("switching next-to-source on and off moves the output with it", async () => {
    const { changeForBeside } = await import("./QueueItemOptions");
    expect(changeForBeside("/games/Astro Bot", true, "/out")).toEqual({ outputDir: "/games", keepBoth: true });
    expect(changeForBeside("/games/Astro Bot", false, " /out ")).toEqual({ outputDir: "/out", keepBoth: undefined });
    expect(changeForBeside("/games/Astro Bot", false, "")).toEqual({ outputDir: undefined, keepBoth: undefined });
    expect(changeForBeside("ps5://10.0.0.2/data/x.exfat", true, "/out")).toEqual({});
  });

  it("while a game is being made, ② and ③ are open and say they are for the next game", () => {
    const make = renderToStaticMarkup(
      <MakeCard make="folder" onMake={() => {}} checked={false} forNext />,
    );
    expect(make).toContain('data-testid="next-game-note"');
    expect(make).not.toMatch(/<button[^>]* disabled=""/);
    expect(make).not.toContain("Check a game under ①");
    const opts = renderToStaticMarkup(
      <OptionsCard
        outputDir="/out"
        onChangeOutput={() => {}}
        onOutputTyped={() => {}}
        canBrowse
        compression="balanced"
        onCompression={() => {}}
        estimates={null}
        locked={false}
        forNext
      >
        <span data-testid="extra" />
      </OptionsCard>,
    );
    expect(opts).toContain('data-testid="next-game-note"');
    expect(opts).toContain('data-testid="extra"');
    expect(opts).not.toMatch(/id="fpkg-output"[^>]*disabled/);
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
    // #669: everything it was added with can change while it waits.
    expect(
      useConvertQueue.getState().update(id, { outputDir: "/mine", compression: "stored", language: "ja", fullVerify: true }),
    ).toBe(true);
    expect(useConvertQueue.getState().items.find((i) => i.id === id)).toMatchObject({
      outputDir: "/mine",
      compression: "stored",
      language: "ja",
      fullVerify: true,
    });
    useConvertQueue.setState({ items: useConvertQueue.getState().items.map((i) => (i.id === id ? { ...i, status: "running" } : i)) });
    expect(useConvertQueue.getState().update(id, { then: "keep" })).toBe(false);
  });
});
