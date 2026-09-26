// Convert a game folder or mount image (.exfat / .ffpkg) into an installable debug FPKG, and
// optionally install it, as three guided cards: ① Game, ② Options, ③ Build & install.
//
// The work runs in the engine (desktop, Docker or Android); the console is only involved at the
// install. `state/fpkgConversion` drives the run; this screen holds the source, its check and the
// remembered options, and hands every action the exact package the run names.

import { useCallback, useEffect, useRef, useState } from "react";

import { PackagePlus } from "lucide-react";

import { fpkg, type FpkgInspection } from "../../api/fpkg";
import { appLaunch } from "../../api/ps5";
import { Callout, Card, PageHeader } from "../../components";
import { transferAddr } from "../../lib/addr";
import { openLocalPath } from "../../lib/openLocalPath";
import { pickPath } from "../../lib/pickPath";
import { isIOS } from "../../lib/platform";
import { isTauriEnv } from "../../lib/tauriEnv";
import { useWebviewDrop } from "../../lib/useWebviewDrop";
import { useConnectionStore } from "../../state/connection";
import { useConvertPrefs } from "../../state/convertPrefs";
import { useFpkgConversion } from "../../state/fpkgConversion";
import { useTr } from "../../state/lang";
import { pickLocalPath } from "../../state/localPicker";
import { useTaskStore } from "../../state/tasks";
import { GameCard, titleIdOf } from "./GameCard";
import { OptionsCard } from "./OptionsCard";
import { RunCard } from "./RunCard";

/** How long a first press of Delete package stays armed. */
const DELETE_ARM_MS = 5000;

function dirOf(path: string): string {
  const i = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return i > 0 ? path.slice(0, i) : path;
}

export default function FpkgConvertScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host) ?? "";
  const payloadUp = useConnectionStore((s) => s.payloadStatus === "up");
  const canInstall = payloadUp && host.trim() !== "";

  const outputDir = useConvertPrefs((s) => s.outputDir);
  const setOutputDir = useConvertPrefs((s) => s.setOutputDir);
  const compression = useConvertPrefs((s) => s.compression);
  const setCompression = useConvertPrefs((s) => s.setCompression);

  const pipeline = useFpkgConversion((s) => s.pipeline);
  const start = useFpkgConversion((s) => s.start);
  const compress = useFpkgConversion((s) => s.compress);
  const retryInstall = useFpkgConversion((s) => s.retryInstall);
  const cancel = useFpkgConversion((s) => s.cancel);
  const reset = useFpkgConversion((s) => s.reset);
  const deletePackage = useFpkgConversion((s) => s.deletePackage);
  const installTaskId = pipeline.phase === "running" ? pipeline.installTaskId : null;
  const installTask = useTaskStore((s) =>
    installTaskId ? (s.tasks.find((t) => t.id === installTaskId) ?? null) : null,
  );

  const locked = pipeline.phase === "running";
  const [source, setSource] = useState(pipeline.phase === "idle" ? "" : pipeline.source);
  const [inspection, setInspection] = useState<FpkgInspection | null>(null);
  const [checking, setChecking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [deleteArmed, setDeleteArmed] = useState(false);
  const disarm = useRef<ReturnType<typeof setTimeout> | null>(null);
  const canBrowse = !isIOS() || !isTauriEnv();

  const check = useCallback(
    async (path: string) => {
      if (!path.trim()) return;
      setChecking(true);
      setError(null);
      setInspection(null);
      try {
        setInspection(await fpkg.inspect(path.trim(), outputDir.trim() || undefined));
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setChecking(false);
      }
    },
    [outputDir],
  );

  /** A new source: the previous result and its buttons go; ignored while a run is going. */
  const chooseSource = useCallback(
    (path: string) => {
      if (useFpkgConversion.getState().pipeline.phase === "running") return;
      reset();
      setDeleteArmed(false);
      setSource(path);
      void check(path);
    },
    [check, reset],
  );

  const dropActive = useWebviewDrop(chooseSource, !locked);

  // The inspection of a restored result's source, so its title id is there for Launch.
  useEffect(() => {
    if (!inspection && source && !checking && pipeline.phase !== "idle") void check(source);
    // Only on mount: later checks come from explicit source changes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => () => {
    if (disarm.current) clearTimeout(disarm.current);
  }, []);

  const browse = useCallback(
    async (mode: "file" | "folder") => {
      try {
        const title =
          mode === "folder"
            ? tr("fpkg.pickFolder", undefined, "Choose the game folder")
            : tr("fpkg.pickImage", undefined, "Choose an .exfat or .ffpkg image");
        const picked = !isTauriEnv()
          ? await pickLocalPath({ mode, title })
          : await pickPath({
              mode,
              title,
              filters:
                mode === "file" ? [{ name: "Game image", extensions: ["exfat", "ffpkg"] }] : undefined,
            });
        if (picked) chooseSource(picked);
      } catch {
        setError(
          tr(
            "fpkg.noPicker",
            undefined,
            "This build cannot browse this machine — type a path the engine can see.",
          ),
        );
      }
    },
    [chooseSource, tr],
  );

  const browseOutput = useCallback(async () => {
    try {
      const picked = !isTauriEnv()
        ? await pickLocalPath({ mode: "folder", title: "Choose an output folder on the engine host" })
        : await pickPath({ mode: "folder", title: "Choose an output folder" });
      if (picked) setOutputDir(picked);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [setOutputDir]);

  const run = (install: boolean) => {
    if (!source.trim()) return;
    setError(null);
    void start(
      { source: source.trim(), outputDir: outputDir.trim() || undefined, compression },
      { install, host: install ? host : null },
    );
  };

  const onDelete = () => {
    if (!deleteArmed) {
      setDeleteArmed(true);
      if (disarm.current) clearTimeout(disarm.current);
      disarm.current = setTimeout(() => setDeleteArmed(false), DELETE_ARM_MS);
      return;
    }
    setDeleteArmed(false);
    void deletePackage().catch((e) => setError(e instanceof Error ? e.message : String(e)));
  };

  const onLaunch = () => {
    const titleId = titleIdOf(inspection?.content_id);
    const target = pipeline.phase === "done" ? pipeline.host : null;
    if (!titleId || !target) return;
    void appLaunch(transferAddr(target), titleId).catch((e) =>
      setError(e instanceof Error ? e.message : String(e)),
    );
  };

  const onAnother = () => {
    reset();
    setDeleteArmed(false);
    setSource("");
    setInspection(null);
    setError(null);
  };

  const isImage = /\.(exfat|ffpkg)$/i.test(source.trim());
  const noFiles = inspection !== null && inspection.files === 0;

  return (
    <div className="mx-auto flex max-w-3xl flex-col gap-4 p-4">
      <PageHeader
        icon={PackagePlus}
        title={tr("fpkg_title", undefined, "Convert to FPKG")}
        description={tr(
          "fpkg.subtitle",
          undefined,
          "Turn a game folder or mount image into an installable package. The work runs on this machine, not the console.",
        )}
      />
      <div className="text-xs text-[var(--color-warn)]">
        {tr(
          "fpkg.betaLine",
          undefined,
          "Beta: a package may fail to install or launch. Keep your original game files.",
        )}
      </div>

      <GameCard
        source={source}
        onSourceTyped={(v) => {
          if (locked) return;
          reset();
          setSource(v);
        }}
        onCheck={() => void check(source)}
        onBrowseFolder={() => void browse("folder")}
        onBrowseImage={() => void browse("file")}
        canBrowse={canBrowse}
        inspection={inspection}
        checking={checking}
        locked={locked}
        dropActive={dropActive}
      />

      <OptionsCard
        outputDir={outputDir}
        onChangeOutput={() => void browseOutput()}
        onOutputTyped={setOutputDir}
        canBrowse={canBrowse}
        compression={compression}
        onCompression={setCompression}
        estimates={inspection?.estimates ?? (inspection ? null : undefined)}
        locked={locked}
      />

      {error && (
        <Callout tone="error" title={tr("fpkg.error", undefined, "Conversion error")}>
          {error}
        </Callout>
      )}

      <RunCard
        pipeline={pipeline}
        installTask={installTask}
        host={host}
        canInstall={canInstall}
        canConvert={!noFiles && !checking && source.trim() !== ""}
        isImage={isImage}
        deleteArmed={deleteArmed}
        onConvert={() => run(false)}
        onConvertInstall={() => run(true)}
        onCompress={() => void compress(source.trim(), outputDir.trim() || undefined)}
        onCancel={() => void cancel()}
        onRetryInstall={() => void retryInstall(host)}
        onLaunch={onLaunch}
        onShowFolder={() => {
          if (pipeline.phase === "done") void openLocalPath(dirOf(pipeline.packagePath));
        }}
        onInstallAgain={() => void retryInstall(host)}
        onDelete={onDelete}
        onAnother={onAnother}
      />

      <details className="text-sm">
        <summary className="cursor-pointer text-[var(--color-muted)]">
          {tr("fpkg.aboutTitle", undefined, "About converting")}
        </summary>
        <Card>
    <div className="flex flex-col gap-2.5 text-sm text-[var(--color-muted)]">
              <p>
                {tr(
                  "fpkg.about",
                  undefined,
                  "Point it at a game folder, or at an .exfat or .ffpkg mount image. The converter reads the tree, checks that everything a launchable package needs is present, and writes a debug-format .pkg into the output folder.",
                )}
              </p>
              <p>
                {tr(
                  "fpkg.aboutWhere",
                  undefined,
                  "The conversion runs on the machine hosting the engine — this computer, a Docker host or an Android device — not on the console. Nothing reaches the console until you press Install, and the install streams the package across rather than staging a copy of it first.",
                )}
              </p>
              <p>
                {tr(
                  "fpkg.aboutSource",
                  undefined,
                  "The source has to be a game tree that is already decrypted. A retail install cannot be unwrapped here, and the check below will say so if that is what you picked.",
                )}
              </p>
              <p>
                {tr(
                  "fpkg.aboutFake",
                  undefined,
                  "The result is a fake package, so the console needs fake-package support loaded before it will install: kstuff (the build with PS5 fake-package support), a53_ppr_install_fast.elf and shadowmountplus.elf, in that order. Install Package states the same thing next to its Install button.",
                )}
              </p>
              <details>
                <summary className="cursor-pointer">
                  {tr("fpkg.aboutChecks", undefined, "What the check looks for")}
                </summary>
                <ul className="mt-1.5 flex list-disc flex-col gap-1 pl-5">
                  <li>
                    {tr(
                      "fpkg.checkEboot",
                      undefined,
                      "An eboot.bin at the root of the tree — the title module. It has to be a raw ELF or a wrapped SELF; a retail-signed module cannot be repackaged.",
                    )}
                  </li>
                  <li>
                    {tr(
                      "fpkg.checkParam",
                      undefined,
                      "sce_sys/param.json, and no sce_sys/param.sfo — a param.sfo sends the console's launch path down the PS4 route.",
                    )}
                  </li>
                  <li>
                    {tr(
                      "fpkg.checkIcons",
                      undefined,
                      "A 36-character content id, both icons, and the rights module sce_sys/about/right.sprx.",
                    )}
                  </li>
                  <li>
                    {tr(
                      "fpkg.checkDrm",
                      undefined,
                      "That the package's DRM value is standard. A free or upgradable value makes the console lock the title; it is rewritten in the package only, and your own file is never touched.",
                    )}
                  </li>
                </ul>
              </details>
            </div>
        </Card>
      </details>
    </div>
  );
}
