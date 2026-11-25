import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../state/lang", () => ({
  useTr: () =>
    (key: string, vars?: Record<string, string | number>, fallback?: string) => {
      let s = fallback ?? key;
      for (const [k, v] of Object.entries(vars ?? {})) s = s.replace(`{${k}}`, String(v));
      return s;
    },
}));

import type { Pipeline } from "../../state/fpkgConversion";
import {
  ImageChoice,
  PacksNote,
  UnpackOption,
  ZstdOption,
  Lz4Option,
  emulatorRule,
  folderLz4Options,
  folderPackOptions,
  folderPacks,
  installRuntimeFor,
  packOptions,
} from "./ImageOptions";
import { amprEmuBuildFor } from "./AmprEmuTool";
import type { FpkgInspection } from "../../api/fpkg";
import { OptionsCard } from "./OptionsCard";
import {
  RunCard,
  deleteLabel,
  usedPacks,
  lz4Active,
  type Lz4Choice,
  type OutputChoiceProps,
  type RunCardProps,
} from "./RunCard";

// No DOM here (vitest in node, no testing-library): render to markup and check what a user
// would see and could press.
const noop = () => {};
/** ③'s one set of choices: a package, kept. */
const choice = (over: Partial<OutputChoiceProps> = {}): OutputChoiceProps => ({
  make: "pkg",
  onMake: noop,
  imageFormat: "ffpkg",
  imageCompress: false,
  onImageChoice: noop,
  then: "keep",
  onThen: noop,
  deleteAfter: false,
  onDeleteAfter: noop,
  canInstall: true,
  ...over,
});
const actions = {
  choice: choice(),
  onRun: noop,
  onCompress: noop,
  onCancel: noop,
  onInstall: noop,
  onLaunch: noop,
  onShowFolder: noop,
  onDelete: noop,
  onAnother: noop,
};

function html(
  pipeline: Pipeline,
  opts: { canInstall?: boolean; isImage?: boolean; title?: string; sourceBytes?: number } = {},
  extra: Partial<RunCardProps> = {},
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
      {...extra}
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
  it("names the console a Convert & install run will install on", () => {
    const running = {
      phase: "running" as const,
      mode: "convert-install" as const,
      source: "/g",
      host: "10.0.0.2",
      stage: "compress" as const,
      stageDone: 1,
      stageTotal: 2,
      startedMs: 0,
      stageStartedMs: 0,
      stageMs: {},
      jobId: "j",
      installTaskId: null,
      taskId: null,
      packagePath: null,
      titleId: null,
      copiedSource: null,
      extractedSource: null,
    };
    const out = renderToStaticMarkup(
      <RunCard
        pipeline={running}
        installTask={null}
        host="10.0.0.9"
        canInstall
        isImage={false}
        deleteArmed={false}
        targetLabel="PS5 Pro"
        {...actions}
      />,
    );
    expect(out).toContain("Will install on PS5 Pro");
  });

  it("hides Show in folder when no file manager can open it", () => {
    const { onShowFolder: _drop, ...rest } = actions;
    void _drop;
    const out = renderToStaticMarkup(
      <RunCard
        pipeline={{ ...done, mode: "convert" }}
        installTask={null}
        host="10.0.0.2"
        canInstall
        isImage={false}
        deleteArmed={false}
        {...rest}
      />,
    );
    expect(button(out, "Show in folder")).toBeNull();
    expect(button(html({ ...done, mode: "convert" }), "Show in folder")).not.toBeNull();
  });

  it("offers only Convert when no console is connected", () => {
    const out = html({ phase: "idle" }, { canInstall: false });
    expect(button(out, "Convert only")?.disabled).toBe(false);
    expect(button(out, "Convert & install")).toBeNull();
    expect(out).toContain("Connect to a PS5 to install");
  });

  it("offers .ffpfsc compression only for an image source", () => {
    expect(button(html({ phase: "idle" }), "Compress to .ffpfsc")).toBeNull();
    expect(button(html({ phase: "idle" }, { isImage: true }), "Compress to .ffpfsc")).not.toBeNull();
  });

  describe("one set of choices for Convert and the queue", () => {
    const idle = (over: Partial<OutputChoiceProps>, more: Record<string, unknown> = {}) =>
      renderToStaticMarkup(
        <RunCard
          pipeline={{ phase: "idle" }}
          installTask={null}
          host="10.0.0.2"
          canInstall
          isImage={false}
          deleteArmed={false}
          {...actions}
          choice={choice(over)}
          onQueue={noop}
          canQueue
          {...more}
        />,
      );
    /** The option labels of the select labelled `label`. */
    const options = (out: string, label: string) => {
      const at = out.indexOf(label);
      const sel = /<select[\s\S]*?<\/select>/.exec(out.slice(at))?.[0] ?? "";
      return [...sel.matchAll(/<option[^>]*>([^<]*)<\/option>/g)].map((m) => m[1].replace(/&amp;/g, "&"));
    };

    it("asks what to do after (what to make is chosen under ②)", () => {
      const out = idle({});
      expect(out).not.toContain(">Make<");
      expect(out).not.toContain("A fake package (FPKG, .pkg)");
      expect(options(out, "After converting")).toEqual(["Keep the package", "Stream & install", "Upload & install"]);
      expect(button(out, "Add this game to the queue")?.disabled).toBe(false);
    });

    it("saves an image to this computer or straight to the PS5", () => {
      const img = { make: "image" as const, onSaveTo: noop };
      expect(options(idle(img, { canImage: true }), "Save to")).toEqual([
        "This computer",
        "The PS5, straight (nothing stored here)",
      ]);
      // A package has no such choice.
      expect(idle({ onSaveTo: noop })).not.toContain("Save to");
      const toPs5 = idle({ ...img, saveTo: "ps5" }, { canImage: true });
      expect(toPs5).not.toContain("After converting");
      expect(toPs5).toContain("goes on where it stopped");
      expect(button(toPs5, "Make game image")?.disabled).toBe(false);
      // An image that can't stream there says why and runs nothing.
      const refused = idle({ ...img, saveTo: "ps5", ps5Refusal: "Save this one to this computer." }, { canImage: true });
      expect(refused).toContain("Save this one to this computer.");
      expect(button(refused, "Make game image")?.disabled).toBe(true);
      // Nor with no console to send to.
      const noPs5 = idle({ ...img, saveTo: "ps5", canInstall: false }, { canImage: true });
      expect(button(noPs5, "Make game image")?.disabled).toBe(true);
    });

    it("saves an image or a game folder straight to a server folder", () => {
      const img = { make: "image" as const, onSaveTo: noop, onServerFolder: noop };
      expect(options(idle(img, { canImage: true }), "Save to")).toEqual([
        "This computer",
        "The PS5, straight (nothing stored here)",
        "A server, straight (nothing stored here)",
      ]);
      // A folder goes to this computer or a server, never straight to the PS5.
      const folder = { make: "folder" as const, onSaveTo: noop, onServerFolder: noop };
      expect(options(idle(folder, { canFolder: true }), "Save to")).toEqual([
        "This computer",
        "A server, straight (nothing stored here)",
      ]);
      // With no folder on a server chosen yet, nothing runs.
      const noFolder = idle({ ...img, saveTo: "server" }, { canImage: true });
      expect(noFolder).toContain('data-testid="server-dest"');
      expect(noFolder).not.toContain("After converting");
      expect(button(noFolder, "Make game image")?.disabled).toBe(true);
      const chosen = idle({ ...img, saveTo: "server", serverFolder: "remote://nas1/games" }, { canImage: true });
      expect(chosen).toContain("read back there before it gets its name");
      expect(button(chosen, "Make game image")?.disabled).toBe(false);
      // An image that can't stream there says why and runs nothing.
      const refused = idle(
        { ...img, saveTo: "server", serverFolder: "remote://nas1/games", serverRefusal: "Not this one." },
        { canImage: true },
      );
      expect(refused).toContain("Not this one.");
      expect(button(refused, "Make game image")?.disabled).toBe(true);
      // A folder build has no image refusal.
      const folderTo = idle(
        { ...folder, saveTo: "server", serverFolder: "remote://nas1/games", serverRefusal: "Not this one." },
        { canFolder: true },
      );
      expect(button(folderTo, "Make game folder")?.disabled).toBe(false);
      // The queue offers no server.
      expect(options(idle({ make: "image", onSaveTo: noop }, { canImage: true }), "Save to")).not.toContain(
        "A server, straight (nothing stored here)",
      );
    });

    it("sends a .ffpfsc straight to the PS5 or a server, saying it is compressed twice", () => {
      const img = { make: "image" as const, onSaveTo: noop, onServerFolder: noop, imageCompress: true };
      const note = 'data-testid="convert-save-to-ffpfsc-note"';
      const toPs5 = idle({ ...img, saveTo: "ps5" }, { canImage: true });
      expect(toPs5).toContain(note);
      expect(toPs5).toContain("compressed twice");
      expect(button(toPs5, "Make compressed image")?.disabled).toBe(false);
      const toServer = idle({ ...img, saveTo: "server", serverFolder: "remote://nas1/games" }, { canImage: true });
      expect(toServer).toContain(note);
      expect(button(toServer, "Make compressed image")?.disabled).toBe(false);
      // Not for a plain image, nor for one saved here.
      expect(idle({ ...img, imageCompress: false, saveTo: "ps5" }, { canImage: true })).not.toContain(note);
      expect(idle({ ...img, saveTo: "computer" }, { canImage: true })).not.toContain(note);
    });

    it("says a finished image is on a server, with nothing to upload, show or delete", () => {
      const out = html(
        {
          phase: "done",
          mode: "image",
          source: "/g",
          host: "10.0.0.2",
          packagePath: "remote://nas1/games/G.exfat",
          packageBytes: 9,
          convertMs: 1,
          installMs: 0,
          stageMs: {},
          deleted: false,
          titleId: null,
        },
        { canInstall: true },
        { onUploadImage: noop },
      );
      expect(out).toContain("written straight to the server");
      expect(out).toContain("nas1 › games/G.exfat");
      expect(button(out, "Upload to PS5")).toBeNull();
      expect(button(out, "Show in folder")).toBeNull();
    });

    it("says what became of a source the run was asked to move to the Trash", () => {
      const image = {
        phase: "done" as const,
        mode: "image" as const,
        source: "/games/PPSA1-app",
        host: null,
        packagePath: "/out/PPSA1-app.ffpkg",
        packageBytes: 9,
        convertMs: 1,
        installMs: 0,
        stageMs: {},
        deleted: false,
        titleId: null,
      };
      // Not asked: nothing is said.
      expect(html(image)).not.toContain("source-note");
      const gone = html({
        ...image,
        sourceNote: { removed: true, note: "source moved to the Trash: /games/PPSA1-app" },
      });
      expect(gone).toContain("The source was moved to the Trash: /games/PPSA1-app");
      const kept = html({
        ...image,
        sourceNote: { removed: false, note: "source kept: /games/PPSA1-app changed while it was being converted" },
      });
      expect(kept).toContain("The source was not moved to the Trash");
      expect(kept).toContain("changed while it was being converted");
    });

    it("says a finished image is on the PS5, with nothing to upload", () => {
      const out = html(
        {
          phase: "done",
          mode: "image",
          source: "/g",
          host: "10.0.0.2",
          packagePath: "/data/homebrew/G.exfat",
          packageBytes: 9,
          convertMs: 1,
          installMs: 0,
          stageMs: {},
          deleted: false,
          titleId: null,
          onConsole: true,
        },
        { canInstall: true },
        { onUploadImage: noop },
      );
      expect(out).toContain("written straight to the PS5");
      expect(out).toContain("/data/homebrew/G.exfat");
      expect(button(out, "Upload to PS5")).toBeNull();
      expect(button(out, "Show in folder")).toBeNull();
    });

    it("names the action after the choice", () => {
      expect(button(idle({}), "Convert only")).not.toBeNull();
      expect(button(idle({ then: "stream" }), "Convert & install")).not.toBeNull();
      expect(button(idle({ then: "upload" }, { replaces: true }), "Convert & replace")).not.toBeNull();
      expect(button(idle({ make: "image" }, { canImage: true }), "Make game image")?.disabled).toBe(false);
      expect(button(idle({ make: "image", imageCompress: true }, { canImage: true }), "Make compressed image")).not.toBeNull();
    });

    it("offers deleting after only when the result is sent somewhere", () => {
      expect(idle({})).not.toContain("Delete the package once installed");
      expect(idle({ then: "stream" })).toContain("Delete the package once installed");
      expect(idle({ make: "image", then: "upload" }, { canImage: true })).toContain(
        "Delete the image once it is on the PS5",
      );
    });

    it("an image is kept or uploaded, never installed, and only a folder here becomes one", () => {
      const out = idle({ make: "image" }, { canImage: false });
      expect(options(out, "After converting")).toEqual(["Keep the image", "Upload it to the PS5"]);
      expect(button(out, "Make game image")?.disabled).toBe(true);
      expect(out).toContain("Only a game folder, game image or archive (.zip, .7z, .rar) with a game in it can become a game image");
      // Queueing still works: a folder of games can be queued as images.
      expect(button(out, "Add this game to the queue")?.disabled).toBe(false);
    });

    it("a folder is only kept, and a folder source cannot become one", () => {
      const ok = idle({ make: "folder" }, { canFolder: true });
      expect(options(ok, "After converting")).toEqual(["Keep the folder"]);
      expect(button(ok, "Make game folder")?.disabled).toBe(false);
      expect(ok).not.toContain("This is already a folder");
      const already = idle({ make: "folder" }, { canFolder: false, isFolderSource: true });
      expect(button(already, "Make game folder")?.disabled).toBe(true);
      expect(already).toContain("This is already a folder");
      const archive = idle({ make: "folder" }, { canFolder: false });
      expect(button(archive, "Make game folder")?.disabled).toBe(true);
      expect(archive).toContain("Only a game image or a package made with the debug key can become a folder");
      // Nothing is sent anywhere, so no console is asked for.
      expect(idle({ make: "folder" }, { canFolder: true, canInstall: false })).not.toContain("Connect to a PS5");
    });
  });

  it("a finished folder shows its path and Show in folder, nothing to install or delete", () => {
    const out = html(
      { ...done, mode: "folder", host: null, packagePath: "/out/Game", titleId: null, installMs: 0 },
      { sourceBytes: 1024 },
    );
    expect(out).toContain("Game folder written and read back.");
    expect(out).toContain("/out/Game");
    expect(button(out, "Show in folder")).not.toBeNull();
    expect(button(out, "Stream & install")).toBeNull();
    expect(button(out, "Delete package")).toBeNull();
    expect(button(out, "View details")).toBeNull();
    expect(button(out, "Upload to PS5")).toBeNull();
    expect(out).not.toContain("% of");
  });

  it("names a folder build's stages for a folder", () => {
    const out = html({
      phase: "running",
      mode: "folder",
      source: "/g/Game.exfat",
      host: null,
      stage: "write",
      stageDone: 1,
      stageTotal: 2,
      startedMs: 0,
      stageStartedMs: 0,
      stageMs: {},
      jobId: "j",
      installTaskId: null,
      taskId: null,
      packagePath: null,
      titleId: null,
      copiedSource: null,
      extractedSource: null,
    });
    expect(out).toContain("Write folder");
    expect(out).toContain("Read the folder back");
    expect(out).not.toContain("Write package");
    expect(button(out, "Cancel")).not.toBeNull();
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
      copiedSource: null,
      extractedSource: null,
      packagePath: null,
      titleId: null,
    });
    for (const label of ["Check source", "Plan package", "Compress", "Write package", "Verify", "Send to PS5", "Install on PS5"]) {
      expect(out).toContain(label);
    }
    expect(button(out, "Cancel")).not.toBeNull();
    expect(button(out, "Convert only")).toBeNull();
  });

  describe("pause (#631)", () => {
    const now = Date.now();
    // The speed is worked out against the clock: hold it still, or a slow run reads 9.89 MiB/s.
    beforeEach(() => {
      vi.useFakeTimers({ toFake: ["Date"] });
      vi.setSystemTime(now);
    });
    afterEach(() => {
      vi.useRealTimers();
    });
    const writing = (over: Partial<Extract<Pipeline, { phase: "running" }>> = {}): Pipeline => ({
      phase: "running",
      mode: "convert",
      source: "/g",
      host: null,
      stage: "write",
      stageDone: 100 * 1024 * 1024,
      stageTotal: 400 * 1024 * 1024,
      startedMs: now - 20_000,
      stageStartedMs: now - 20_000,
      stageMs: {},
      jobId: "j",
      installTaskId: null,
      taskId: null,
      packagePath: null,
      titleId: null,
      copiedSource: null,
      extractedSource: null,
      ...over,
    });
    const pausable = { onPause: noop, onResume: noop };

    it("offers Pause beside Cancel while the job runs, and Resume once paused", () => {
      let out = html(writing(), {}, pausable);
      expect(button(out, "Pause")).not.toBeNull();
      expect(button(out, "Resume")).toBeNull();
      expect(button(out, "Cancel")).not.toBeNull();
      out = html(writing({ paused: true, pausedAtMs: now - 5000 }), {}, pausable);
      expect(button(out, "Resume")).not.toBeNull();
      expect(button(out, "Cancel")).not.toBeNull();
      expect(out).toContain("Paused — the queue waits");
      // No speed or time left while nothing moves.
      expect(out).not.toContain("left");
      expect(out).not.toMatch(/iB\/s/);
    });

    it("has no Pause where the caller cannot pause, or before the job exists", () => {
      expect(button(html(writing()), "Pause")).toBeNull();
      expect(button(html(writing({ jobId: null }), {}, pausable), "Pause")).toBeNull();
      expect(button(html(writing({ stage: "copy" }), {}, pausable), "Pause")).toBeNull();
    });

    it("leaves paused time out of the speed", () => {
      // 100 MiB in 20 s, 10 of them paused: 10 MiB/s, not 5.
      const out = html(writing({ stagePausedMs: 10_000, pausedMs: 10_000 }), {}, pausable);
      expect(out).toMatch(/· (9\.9\d|10\.0) MiB\/s/);
    });

    it("shows the Use less of this computer switch while a build runs", () => {
      expect(html(writing(), {}, pausable)).toContain("Use less of this computer");
    });
  });

  it("names the kept package and offers both install routes after an install failure", () => {
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
    expect(button(out, "Stream & install")).not.toBeNull();
    expect(button(out, "Upload & install")).not.toBeNull();
  });

  it("offers mount and install after a failed run only for a kept .pkg, never an image", () => {
    const failed = (packagePath: string, mode: "convert-install" | "image") =>
      html(
        {
          phase: "failed",
          mode,
          source: "/g",
          host: "10.0.0.2",
          stage: "install",
          message: "unreachable",
          packagePath,
          stageMs: {},
          titleId: null,
        },
        {},
        { kraken: true, onMount: noop },
      );
    expect(button(failed("/out/a.pkg", "convert-install"), "Put on the PS5 to mount")).not.toBeNull();
    const image = failed("/out/a.ffpfsc", "image");
    expect(button(image, "Put on the PS5 to mount")).toBeNull();
    expect(button(image, "Stream & install")).toBeNull();
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
    expect(button(out, "Stream & install")).toBeNull();
    expect(button(out, "Upload & install")).toBeNull();
    expect(out).not.toContain("built and kept");
  });

  it("shows the result actions after an install, and no Launch for Convert only", () => {
    const installed = html({ ...done, mode: "convert-install" });
    for (const name of ["Launch on PS5", "Stream & install", "Upload & install", "Delete package", "Convert another game"]) {
      expect(button(installed, name)).not.toBeNull();
    }
    expect(button(html({ ...done, mode: "convert" }), "Launch on PS5")).toBeNull();
  });

  it("follows Convert only with a choice of stream or upload install, each needing a console", () => {
    const converted = html({ ...done, mode: "convert" });
    expect(button(converted, "Stream & install")).toMatchObject({ disabled: false });
    expect(button(converted, "Upload & install")).toMatchObject({ disabled: false });
    expect(converted).toContain("copies it to the PS5 first");
    const offline = html({ ...done, mode: "convert" }, { canInstall: false });
    expect(button(offline, "Stream & install")).toMatchObject({ disabled: true });
    expect(button(offline, "Upload & install")).toMatchObject({ disabled: true });
    expect(button(html({ ...done, mode: "image" }), "Upload & install")).toBeNull();
  });

  it("drops the package actions once the package is deleted", () => {
    const out = html({ ...done, mode: "convert-install", deleted: true });
    expect(out).toContain("Package deleted");
    for (const name of ["Stream & install", "Upload & install", "Delete package", "Launch on PS5"]) {
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
      copiedSource: null,
      extractedSource: null,
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

  describe("AMPR LZ4 asset packs", () => {
    const lz4 = (over: Partial<Lz4Choice> = {}): Lz4Choice => ({
      readiness: { runtime_version: "0.4.2.1", refusal: null },
      on: false,
      onToggle: noop,
      profileName: null,
      onProfile: noop,
      kind: "lz4",
      onKind: noop,
      zstdLevel: 5,
      onZstdLevel: noop,
      ...over,
    });
    // The image's options live under ③ (ImageChoice); the run card only runs.
    const idle = (format: "exfat" | "ffpkg", lz4Choice: Lz4Choice | undefined, compress = false) =>
      renderToStaticMarkup(<ImageChoice format={format} compress={compress} onChoice={noop} lz4={lz4Choice} />);
    /** The checkbox in the label whose text contains `text`. */
    const checkbox = (out: string, text: string) => {
      const re = /<label[^>]*>([\s\S]*?)<\/label>/g;
      for (let m = re.exec(out); m; m = re.exec(out)) {
        if (m[1].replace(/<[^>]+>/g, "").includes(text)) {
          const input = /<input([^>]*)>/.exec(m[1])?.[1] ?? "";
          return { checked: /\schecked=""/.test(input), disabled: /\sdisabled=""/.test(input) };
        }
      }
      return null;
    };

    it("is offered for an AMPR game's image in every format", () => {
      for (const format of ["exfat", "ffpkg"] as const) {
        expect(idle(format, lz4())).toContain("AMPR LZ4 asset packs");
        expect(idle(format, lz4())).toContain("ampr_emu: 0.4.2.1");
        expect(idle(format, lz4())).toContain("Works with every image format, and inside a .ffpfsc.");
      }
      expect(idle("exfat", undefined)).not.toContain("AMPR LZ4 asset packs");
    });

    it("LZ4 packs for a game whose emulator can't read them always bring ampr_emu, with no box to untick (#828)", () => {
      const calls: string[] = [];
      const readiness = { runtime_version: null, refusal: "none", kind: "runtime" as const };
      const choice = lz4({
        on: true,
        readiness,
        onToggle: (on) => calls.push(`packs:${on}`),
        onKind: (k) => calls.push(`kind:${k}`),
      });
      /** Every checkbox in the element tree, with the testid it is under. */
      const inputs: { onChange: (e: { target: { checked: boolean } }) => void; testid?: string }[] = [];
      const walk = (node: unknown, testid?: string) => {
        if (Array.isArray(node)) return node.forEach((n) => walk(n, testid));
        if (!node || typeof node !== "object" || !("props" in node)) return;
        const props = (node as { props: Record<string, unknown> }).props;
        const id = (props["data-testid"] as string | undefined) ?? testid;
        if (props.type === "checkbox") inputs.push({ onChange: props.onChange as never, testid: id });
        walk(props.children, id);
      };
      walk(Lz4Option({ lz4: choice }));
      // One checkbox: the packs. ampr_emu goes in with them.
      expect(inputs.map((i) => i.testid)).toEqual(["lz4-option"]);
      inputs[0].onChange({ target: { checked: true } });
      expect(calls).toEqual(["kind:lz4", "packs:true"]);
      expect(installRuntimeFor(choice)).toBe(true);
      expect(lz4Active(choice)).toBe(true);
      expect(packOptions(choice)).toEqual({ installRuntime: true });
      expect(folderLz4Options(choice)).toEqual({ profileToml: undefined, installRuntime: true });
      expect(emulatorRule(choice)).toEqual({ kind: "fixed", build: "ampr_emu", added: "always" });
      const out = renderToStaticMarkup(<Lz4Option lz4={choice} />);
      expect(out).toContain('data-testid="lz4-install-runtime"');
      expect(out).toContain("The image gets drakmor&#x27;s ampr_emu 0.4.2.1");
      // A game whose own ampr_emu reads packs gets nothing put in.
      expect(installRuntimeFor(lz4({ on: true }))).toBe(false);
      expect(emulatorRule(lz4({ on: true }))).toEqual({ kind: "none" });
    });

    it("goes with .ffpfsc compression, and offers a profile", () => {
      const on = idle("ffpkg", lz4({ on: true }), true);
      expect(checkbox(on, "Compress it into a .ffpfsc")).toEqual({ checked: true, disabled: false });
      expect(checkbox(on, "AMPR LZ4 asset packs")).toEqual({ checked: true, disabled: false });
      // The compression level is offered with the packs.
      expect(on).toContain('data-testid="ffpfsc-level"');
      expect(on).toContain("Use a profile (.toml)");
      const named = idle("exfat", lz4({ on: true, profileName: "sm2.toml" }));
      expect(named).toContain("Profile: sm2.toml");
      expect(button(named, "Use the built-in profile")).not.toBeNull();
      expect(lz4Active(lz4({ on: true }))).toBe(true);
      expect(packOptions(lz4({ on: true }))).toEqual({ installRuntime: false });
    });

    it("offers zstd on any image, needing no emulator of the game's own", () => {
      const noEmulator = { runtime_version: null, refusal: "no emulator", kind: "runtime" as const };
      const zstd = lz4({ on: true, kind: "zstd", readiness: noEmulator });
      expect(lz4Active(zstd)).toBe(true);
      // zstd packs bring ampr-emu-ng, the only emulator that reads them: never drakmor's.
      expect(emulatorRule(zstd)).toEqual({ kind: "fixed", build: "ampr-emu-ng", added: "always" });
      // LZ4 for that game brings ampr_emu 0.4.2.1.
      expect(lz4Active(lz4({ on: true, readiness: noEmulator }))).toBe(true);
      expect(checkbox(idle("ffpkg", zstd), "zstd asset packs")).not.toBeNull();
      expect(checkbox(idle("ffpkg", zstd), "AMPR LZ4 asset packs")?.checked).toBe(false);
      // Inside a .ffpfsc too.
      expect(checkbox(idle("ffpkg", zstd, true), "Compress it into a .ffpfsc")).toEqual({
        checked: true,
        disabled: false,
      });
      expect(checkbox(idle("exfat", zstd), "AMPR LZ4 asset packs")?.checked).toBe(false);
      expect(checkbox(idle("exfat", zstd), "zstd asset packs")?.checked).toBe(true);
      // A game that doesn't read through AMPR is packed too (ampr-emu-ng goes in all the same),
      // marked as untested whether the box is ticked or not.
      const untested = "zstd packs for such games are new and not tested on every game";
      const notAmpr = lz4({ kind: "zstd", on: true, readiness: { refusal: "no", kind: "no_ampr" } });
      expect(lz4Active(notAmpr)).toBe(true);
      expect(checkbox(idle("ffpkg", notAmpr), "zstd asset packs")?.checked).toBe(true);
      expect(idle("ffpkg", notAmpr)).toContain(untested);
      expect(idle("ffpkg", { ...notAmpr, on: false })).toContain(untested);
      expect(idle("ffpkg", zstd)).not.toContain(untested);
      // A PS4 game gets no packs: ampr-emu-ng is a PS5 library.
      const ps4 = { ...notAmpr, ps4: true };
      expect(lz4Active(ps4)).toBe(false);
      // The resume note also names zstd asset packs, so look for the option itself.
      expect(checkbox(idle("ffpkg", ps4), "zstd asset packs")).toBeNull();
      // The runtime choice goes with it: the newest release unless the tested one is chosen.
      expect(packOptions(zstd)).toEqual({ zstdLevel: 5, zstdRuntime: "latest" });
      // A profile picked for zstd packs goes with them: it decides what stays loose.
      expect(packOptions(zstd, "t")).toEqual({ zstdLevel: 5, zstdRuntime: "latest", profileToml: "t" });
      expect(idle("ffpkg", zstd)).toContain("Use a profile (.toml)");
      expect(packOptions(lz4())).toBeUndefined();
      expect(packOptions(lz4({ on: true }), "t")).toEqual({ profileToml: "t", installRuntime: false });
      expect(idle("ffpkg", zstd)).toContain("Newest release (default)");
      expect(idle("ffpkg", zstd)).toContain("Tested 0.4.1");
    });

    it("packs a folder in zstd, with the runtime chosen, or in LZ4", () => {
      const zstd = lz4({ on: true, kind: "zstd" });
      expect(folderPacks(zstd)).toBe(true);
      expect(folderPackOptions(zstd)).toEqual({ level: 5, runtime: "latest" });
      expect(folderLz4Options(zstd)).toBeUndefined();
      // LZ4 packs go into a folder too, as into any image.
      expect(folderPacks(lz4({ on: true }))).toBe(true);
      expect(folderPackOptions(lz4({ on: true }))).toBeUndefined();
      expect(folderLz4Options(lz4({ on: true }), "t")).toEqual({ profileToml: "t", installRuntime: false });
      expect(
        folderLz4Options(lz4({ on: true, readiness: { runtime_version: null, refusal: "old", kind: "runtime" } })),
      ).toEqual({
        profileToml: undefined,
        installRuntime: true,
      });
      expect(folderPackOptions(undefined)).toBeUndefined();
      expect(folderLz4Options(undefined)).toBeUndefined();
      const folderLz4 = renderToStaticMarkup(<Lz4Option lz4={lz4({ on: true })} folder />);
      expect(folderLz4).toContain("Writes the game as a plain folder with its data in LZ4 asset packs");
      const runtimeReadiness = { runtime_version: null, refusal: "none", kind: "runtime" as const };
      const runtime = renderToStaticMarkup(
        <Lz4Option lz4={lz4({ on: true, readiness: runtimeReadiness })} folder />,
      );
      expect(runtime).toContain("The folder gets drakmor&#x27;s ampr_emu 0.4.2.1");
      expect(runtime).not.toContain("Not available for this game");
      // Only under LZ4 packs picked (#828).
      expect(renderToStaticMarkup(<Lz4Option lz4={lz4({ readiness: runtimeReadiness })} folder />)).not.toContain(
        "ampr_emu 0.4.2.1",
      );
      const notAmpr = lz4({ kind: "zstd", on: true, readiness: { refusal: "no", kind: "no_ampr" } });
      expect(folderPacks(notAmpr)).toBe(true);
      expect(renderToStaticMarkup(<ZstdOption lz4={notAmpr} folder />)).toContain(
        "keep the original until you have started the packed one",
      );
      expect(folderPacks({ ...notAmpr, ps4: true })).toBe(false);
      expect(folderPackOptions(zstd, "t")).toEqual({ level: 5, runtime: "latest", profileToml: "t" });
      // A picked profile is named, with what it means for zstd; a measured one is LZ4's.
      const picked = renderToStaticMarkup(<ZstdOption lz4={{ ...zstd, profileName: "mine.toml" }} folder />);
      expect(picked).toContain("Profile: mine.toml");
      expect(picked).toContain("only decides which files stay loose");
      const measured = renderToStaticMarkup(
        <ZstdOption lz4={{ ...zstd, profileName: "measured", profileMeasured: true }} folder />,
      );
      expect(measured).not.toContain("Profile: measured");
      expect(measured).toContain("Use a profile (.toml)");
      const html = renderToStaticMarkup(<ZstdOption lz4={zstd} folder />);
      expect(html).not.toContain("LZ4 packs are not offered here");
      expect(html).toContain('data-testid="zstd-runtime"');
      // ③ for a folder: both pack options for an AMPR game, zstd alone for one that isn't.
      const folderCard = (choice: Lz4Choice) =>
        renderToStaticMarkup(
          <OptionsCard
            outputDir=""
            onChangeOutput={noop}
            onOutputTyped={noop}
            canBrowse={false}
            compression="balanced"
            onCompression={noop}
            estimates={null}
            locked={false}
            make="folder"
            image={{ format: "ffpkg", compress: false, onChoice: noop, lz4: choice }}
          />,
        );
      const ampr = folderCard(lz4());
      expect(ampr).toContain('data-testid="lz4-option"');
      expect(ampr).toContain('data-testid="zstd-option"');
      const plain = folderCard(lz4({ readiness: { refusal: "no", kind: "no_ampr" } }));
      expect(plain).not.toContain('data-testid="lz4-option"');
      expect(plain).toContain('data-testid="zstd-option"');
      expect(plain).toContain("This game doesn&#x27;t use AMPR");
      // A folder never shows the image's emulator switch.
      expect(folderCard(lz4({ readiness: { refusal: "x", kind: "runtime", emulator_missing: true } }))).not.toContain(
        'data-testid="ampr-emu-option"',
      );
    });

    it("shows only the emulator choice that applies (#828)", () => {
      // A game that reads through AMPR and ships no emulator: its own can't read LZ4 packs.
      const readiness = { runtime_version: null, refusal: "no emulator", kind: "runtime" as const, emulator_missing: true };
      const toggle = 'data-testid="ampr-emu-option"';
      const putRow = 'data-testid="lz4-install-runtime"';
      const ngLine = "The zstd runtime (ampr-emu-ng) goes in with the packs, so no other emulator is needed.";
      // No packs: the plain-image emulator toggle, said to be for an image without packs.
      const plain = idle("ffpkg", lz4({ readiness }));
      expect(plain).toContain(toggle);
      expect(plain).toContain("For an image without asset packs");
      expect(plain).not.toContain(putRow);
      expect(plain).not.toContain(ngLine);
      // LZ4 can still be picked: it puts ampr_emu 0.4.2.1 in, and that row shows under it.
      expect(checkbox(plain, "AMPR LZ4 asset packs")).toEqual({ checked: false, disabled: false });
      const lz4Picked = idle("ffpkg", lz4({ on: true, readiness }));
      expect(lz4Picked).toContain(putRow);
      expect(checkbox(lz4Picked, "AMPR LZ4 asset packs")?.checked).toBe(true);
      expect(lz4Picked).not.toContain(toggle);
      expect(lz4Picked).not.toContain(ngLine);
      // zstd packs: neither drakmor's toggle nor the LZ4 row, one line on ampr-emu-ng instead.
      const zstd = idle("ffpkg", lz4({ on: true, kind: "zstd", readiness }));
      expect(zstd).not.toContain(toggle);
      expect(zstd).not.toContain(putRow);
      expect(zstd).toContain(ngLine);
      expect(renderToStaticMarkup(<ZstdOption lz4={lz4({ on: true, kind: "zstd" })} folder />)).toContain(ngLine);
      expect(renderToStaticMarkup(<ZstdOption lz4={lz4({ kind: "zstd" })} />)).not.toContain(ngLine);
      // zstd is never offered with drakmor's emulator: no emulator picker at all with it.
      expect(zstd).not.toContain('data-testid="ampr-emu-build"');
      // The packs come first, then the emulator that goes with them.
      expect(plain.indexOf('data-testid="lz4-option"')).toBeLessThan(plain.indexOf(toggle));
      expect(plain.indexOf('data-testid="zstd-option"')).toBeLessThan(plain.indexOf(toggle));
      expect(emulatorRule(lz4({ readiness }))).toEqual({ kind: "choose" });
    });

    it("a game kept in the packs it is in gets only the emulator that reads them (#828)", () => {
      const readiness = { runtime_version: null, refusal: "no emulator", kind: "runtime" as const, emulator_missing: true };
      expect(emulatorRule(lz4({ readiness, packed: "zstd" }))).toEqual({
        kind: "fixed",
        build: "ampr-emu-ng",
        added: "switch",
      });
      expect(emulatorRule(lz4({ readiness, packed: "lz4" }))).toEqual({ kind: "fixed", build: "ampr_emu", added: "switch" });
      // Written out as plain files: either emulator reads them.
      expect(emulatorRule(lz4({ readiness, packed: "zstd", unpacking: true }))).toEqual({ kind: "choose" });
      const kept = idle("ffpkg", lz4({ readiness, packed: "zstd" }));
      expect(kept).toContain('data-testid="ampr-emu-fixed"');
      expect(kept).toContain("This game stays in its zstd packs, so it gets ampr-emu-ng");
      expect(kept).not.toContain('data-testid="ampr-emu-build"');
      expect(idle("ffpkg", lz4({ readiness, packed: "zstd", unpacking: true }))).toContain('data-testid="ampr-emu-build"');
      // In place, too: Workshop's "Add the AMPR emulator" follows the packs.
      expect(amprEmuBuildFor({ packs: "zstd" } as FpkgInspection, "ampr_emu")).toBe("ampr-emu-ng");
      expect(amprEmuBuildFor({ packs: "lz4" } as FpkgInspection, "ampr-emu-ng")).toBe("ampr_emu");
      expect(amprEmuBuildFor({ packs: null } as FpkgInspection, "ampr-emu-ng")).toBe("ampr-emu-ng");
    });

    it("shows no AMPR choice at all for a game that doesn't use AMPR (#828)", () => {
      const notAmpr = lz4({ readiness: { refusal: "no", kind: "no_ampr" } });
      expect(emulatorRule(notAmpr)).toEqual({ kind: "none" });
      expect(emulatorRule({ ...notAmpr, packed: "zstd" })).toEqual({ kind: "none" });
      const out = idle("ffpkg", notAmpr);
      expect(out).toContain('data-testid="ampr-not-used"');
      for (const id of ["lz4-option", "ampr-emu-option", "ampr-emu-build", "lz4-install-runtime"]) {
        expect(out).not.toContain(`data-testid="${id}"`);
      }
      // Its zstd packs bring their own runtime: still no emulator choice.
      const zstd = idle("ffpkg", { ...notAmpr, on: true, kind: "zstd" });
      expect(zstd).not.toContain('data-testid="ampr-emu-option"');
      expect(zstd).not.toContain('data-testid="ampr-emu-build"');
    });

    it("offers to write a packed game as plain files again", () => {
      const html = renderToStaticMarkup(<UnpackOption unpack={{ kind: "zstd", on: true, onToggle: () => {} }} />);
      expect(html).toContain("Unpack the asset packs (plain files again)");
      expect(html).toContain("This game is in zstd asset packs");
      expect(checkbox(html, "Unpack the asset packs")?.checked).toBe(true);
      expect(
        renderToStaticMarkup(<UnpackOption unpack={{ kind: "lz4", on: false, onToggle: () => {} }} />),
      ).toContain("This game is in LZ4 asset packs");
    });

    it("says why packs aren't offered instead of hiding them", () => {
      // No game checked yet: where they'll appear (Convert's ③ only, not a queued game).
      const pending = renderToStaticMarkup(
        <ImageChoice format="ffpkg" compress={false} onChoice={noop} packsNote />,
      );
      expect(pending).toContain("Asset packs (LZ4, zstd) appear here once a PS5 game is checked.");
      expect(idle("ffpkg", undefined)).not.toContain('data-testid="packs-note"');
      expect(renderToStaticMarkup(<PacksNote lz4={undefined} />)).toContain("Asset packs (LZ4, zstd) appear here");
      // A game that doesn't read through AMPR: one line says so, and zstd is offered.
      const notAmpr = lz4({ readiness: { refusal: "no", kind: "no_ampr" } });
      for (const format of ["exfat", "ffpkg"] as const) {
        const out = idle(format, notAmpr);
        expect(out).toContain("This game doesn&#x27;t use AMPR, so it needs no AMPR emulator.");
        expect(out).not.toContain('data-testid="lz4-option"');
        expect(out).toContain('data-testid="zstd-option"');
      }
      expect(renderToStaticMarkup(<PacksNote lz4={notAmpr} />)).toBe("");
      // A PS4 game: one line, no options.
      const ps4 = { ...notAmpr, ps4: true };
      for (const format of ["exfat", "ffpkg"] as const) {
        const out = idle(format, ps4);
        expect(out).toContain("Asset packs: not offered, this game doesn&#x27;t read its files through AMPR.");
        expect(out).not.toContain('data-testid="lz4-option"');
        expect(out).not.toContain('data-testid="zstd-option"');
      }
      expect(renderToStaticMarkup(<PacksNote lz4={ps4} />)).toContain("Asset packs: not offered");
      // An AMPR game: both pack options, in any format, and nothing to explain.
      for (const format of ["exfat", "ffpkg"] as const) {
        const out = idle(format, lz4());
        expect(out).toContain('data-testid="lz4-option"');
        expect(out).toContain('data-testid="zstd-option"');
        expect(out).not.toContain('data-testid="packs-note"');
      }
      expect(renderToStaticMarkup(<PacksNote lz4={lz4()} />)).toBe("");
    });

    it("warns that a packed game needs ShadowMount+ while a pack option is on", () => {
      const warning = "A packed game only plays when ShadowMount+ mounts it.";
      expect(idle("exfat", lz4())).not.toContain(warning);
      expect(idle("exfat", lz4({ on: true }))).toContain(warning);
      expect(idle("ffpkg", lz4({ on: true, kind: "zstd" }))).toContain(warning);
      expect(renderToStaticMarkup(<ZstdOption lz4={lz4({ on: true, kind: "zstd" })} folder />)).toContain(warning);
      // Not under LZ4 that the game refuses.
      const refused = lz4({ on: true, readiness: { runtime_version: null, refusal: "x", kind: "ok" } });
      expect(idle("exfat", refused)).not.toContain(warning);
    });

    it("explains which image to make, in a disclosure anyone can open", () => {
      const out = idle("ffpkg", undefined);
      expect(out).toMatch(/<details[^>]*data-testid="which-image"[^>]*><summary[^>]*>[\s\S]*Which image should I make\?/);
      for (const name of [".ffpkg (UFS2)", ".exfat", ".ffpfs (PFS)", ".ffpfsc", "Asset packs (LZ4: AMPR games only)"]) {
        expect(out).toContain(name);
      }
      expect(out).toContain("so a packed game must be mounted by ShadowMount+.");
    });

    it("tells a finished packed build to mount it with ShadowMount+", () => {
      const note = "Mount this image with ShadowMount+";
      const image = { ...done, mode: "image" as const, packagePath: "/out/a.exfat" };
      expect(html({ ...image, stageMs: { pack: 10, write: 20 } })).toContain(note);
      expect(html({ ...image, stageMs: { write: 20 } })).not.toContain(note);
      expect(
        html({ ...done, mode: "folder" as const, packagePath: "/out/g", stageMs: { pack: 10 } }),
      ).toContain("Mount this folder with ShadowMount+");
      expect(usedPacks({ mode: "convert", stageMs: { pack: 1 } })).toBe(false);
    });

    it("puts ampr_emu in, saying why, when the game's own cannot serve packs (#449, #828)", () => {
      // An older engine gives the refusal without a kind: the same case.
      const old = lz4({
        on: true,
        readiness: { runtime_version: null, refusal: "the game's ampr_emu does not serve asset packs" },
      });
      const out = idle("exfat", old);
      expect(checkbox(out, "AMPR LZ4 asset packs")).toEqual({ checked: true, disabled: false });
      expect(out).toContain("the game&#x27;s ampr_emu does not serve asset packs. The image gets drakmor&#x27;s ampr_emu 0.4.2.1");
      expect(out).not.toContain("Not available for this game");
      expect(lz4Active(old)).toBe(true);
      expect(packOptions(old)).toEqual({ installRuntime: true });
    });
  });
});
