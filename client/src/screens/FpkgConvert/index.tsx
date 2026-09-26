// Convert a game folder or mount image (.exfat / .ffpkg) into an installable
// debug FPKG, then hand the result straight to the stream installer.
//
// Everything happens in the engine — desktop, Docker or Android; the console is
// only involved at the install step. The engine's work runs as a job, so this
// screen starts one and polls it like the transfer screens do.

import { useCallback, useState } from "react";

import { PackagePlus } from "lucide-react";

import {
  Button,
  Callout,
  Card,
  ConnectionGate,
  Input,
  PageHeader,
  ProgressBar,
  SegmentedControl,
} from "../../components";
import { fpkg, type FpkgCompression, type FpkgInspection } from "../../api/fpkg";
import { pickPath } from "../../lib/pickPath";
import { pickLocalPath } from "../../state/localPicker";
import { isIOS } from "../../lib/platform";
import { isTauriEnv } from "../../lib/tauriEnv";
import { useConnectionStore } from "../../state/connection";
import { useFpkgConversion } from "../../state/fpkgConversion";
import { usePkgLibrary } from "../../state/pkgLibrary";
import { useTr } from "../../state/lang";

function prettyBytes(n: number): string {
  if (n >= 1 << 30) return `${(n / (1 << 30)).toFixed(2)} GiB`;
  if (n >= 1 << 20) return `${(n / (1 << 20)).toFixed(1)} MiB`;
  return `${n} B`;
}

/** The most free space a conversion can need on the output drive: the package, which the
 *  compressed image is written straight into, plus a little headroom. Compression usually
 *  makes it far smaller; this is the bound for a game whose data does not compress. */
function convertSpaceNeeded(plannedSize: number): number {
  return Math.ceil(plannedSize * 1.02);
}

export default function FpkgConvertScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const installStream = usePkgLibrary(host, (s) => s.installStream);

  const [source, setSource] = useState("");
  const [outputDir, setOutputDir] = useState("");
  const [inspection, setInspection] = useState<FpkgInspection | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const jobId = useFpkgConversion((s) => s.jobId);
  const job = useFpkgConversion((s) => s.job);
  const jobError = useFpkgConversion((s) => s.error);
  const starting = useFpkgConversion((s) => s.starting);
  const startConversion = useFpkgConversion((s) => s.start);
  const startCompression = useFpkgConversion((s) => s.compress);
  const kind = useFpkgConversion((s) => s.kind);
  const cancelConversion = useFpkgConversion((s) => s.cancel);
  const [compression, setCompression] = useState<FpkgCompression>("balanced");
  const [installing, setInstalling] = useState(false);
  const [installResult, setInstallResult] = useState<string | null>(null);

  const check = useCallback(
    async (path: string, out: string) => {
      if (!path.trim()) return;
      setBusy(true);
      setError(null);
      setInspection(null);
      try {
        setInspection(await fpkg.inspect(path.trim(), out.trim() || undefined));
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setBusy(false);
      }
    },
    [],
  );

  const browse = useCallback(
    async (mode: "file" | "folder") => {
      try {
        const opts = {
          mode,
          title:
            mode === "folder"
              ? tr("fpkg.pickFolder", undefined, "Choose the game folder")
              : tr("fpkg.pickImage", undefined, "Choose an .exfat or .ffpkg image"),
          filters:
            mode === "file"
              ? [{ name: "Game image", extensions: ["exfat", "ffpkg"] }]
              : undefined,
        } as const;
        const picked = !isTauriEnv()
          ? await pickLocalPath({ mode, title: opts.title })
          : await pickPath(opts);
        if (picked) {
          setSource(picked);
          void check(picked, outputDir);
        }
      } catch {
        // No native dialog (the browser build reads the engine's own disk):
        // the text field and the server-side picker are the way in there.
        setError(
          tr(
            "fpkg.noPicker",
            undefined,
            "This build cannot browse this machine — type a path the engine can see.",
          ),
        );
      }
    },
    [check, outputDir, tr],
  );

  const convert = useCallback(async () => {
    if (!source.trim()) return;
    setError(null);
    setInstallResult(null);
    await startConversion({
      source: source.trim(),
      outputDir: outputDir.trim() || undefined,
      compression,
    });
  }, [source, outputDir, compression, startConversion]);

  const compress = useCallback(async () => {
    if (!source.trim()) return;
    setError(null);
    setInstallResult(null);
    await startCompression(source.trim(), outputDir.trim() || undefined);
  }, [source, outputDir, startCompression]);

  const install = useCallback(async () => {
    if (!job?.dest || !host?.trim()) return;
    setInstalling(true);
    setError(null);
    try {
      const r = await installStream(job.dest, host);
      setInstallResult(
        r.ok
          ? tr("fpkg.installStarted", undefined, "Install handed to the console.")
          : (r.message ?? tr("fpkg.installFailed", undefined, "Install failed.")),
      );
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setInstalling(false);
    }
  }, [job, host, installStream, tr]);

  const running = job?.status === "running" || jobId !== null || starting;
  // Compression takes one image file; a game folder has nothing to wrap.
  const isImage = /\.(exfat|ffpkg)$/i.test(source.trim());
  const warnings = inspection?.checks.filter((c) => !c.ok) ?? [];
  const passed = inspection?.checks.filter((c) => c.ok) ?? [];
  // Desktop and Android can open a real-path picker; the browser build reads
  // the engine's disk instead, where a typed path (or the server picker) is
  // the only route.
  const canBrowse = !isIOS() || !isTauriEnv();

  const browseOutput = useCallback(async () => {
    try {
      const picked = !isTauriEnv()
        ? await pickLocalPath({ mode: "folder", title: "Choose an output folder on the engine host" })
        : await pickPath({ mode: "folder", title: "Choose an output folder" });
      if (picked) setOutputDir(picked);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

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

      {/* The honest state of the feature, above everything else on the screen.
          A user who converts a 60 GB game and finds it will not launch should
          have been told that up front, not left to infer it. */}
      <Callout
        tone="warn"
        title={tr("fpkg.betaTitle", undefined, "Beta — still being built")}
      >
        {tr(
          "fpkg.betaBody",
          undefined,
          "Work in progress: conversion or installation may fail, and a package may install but not launch. Keep your original source and backups. Do not use this on irreplaceable data; we cannot guarantee against data loss or damage.",
        )}
      </Callout>

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

      <Card>
        <div className="flex flex-col gap-2">
          <Input
            id="fpkg-source"
            label={tr("fpkg.source", undefined, "Game source")}
            placeholder={tr(
              "fpkg.sourcePlaceholder",
              undefined,
              "/games/PPSA09519.exfat, /games/my-game, /games/game.ffpkg",
            )}
            value={source}
            onChange={(e) => setSource(e.target.value)}
            onBlur={() => void check(source, outputDir)}
          />
          <div className="flex flex-wrap gap-2">
            {canBrowse && (
              <>
                <Button onClick={() => void browse("folder")} disabled={busy}>
                  {tr("fpkg.browseFolder", undefined, "Browse folder…")}
                </Button>
                <Button onClick={() => void browse("file")} disabled={busy}>
                  {tr("fpkg.browseImage", undefined, "Browse image…")}
                </Button>
              </>
            )}
            <Button onClick={() => void check(source, outputDir)} disabled={busy || !source.trim()}>
              {tr("fpkg.check", undefined, "Check")}
            </Button>
          </div>
          <div className="mt-2">
            <Input
              id="fpkg-output"
              label={tr("fpkg.output", undefined, "Output folder")}
              placeholder={tr(
                "fpkg.outputHint",
                undefined,
                "Default: ~/Downloads/fpkgs",
              )}
              value={outputDir}
              onChange={(e) => setOutputDir(e.target.value)}
            />
            {canBrowse && (
              <Button onClick={() => void browseOutput()}>
                {tr("fpkg.browseOutput", undefined, "Browse output folder…")}
              </Button>
            )}
            <div className="mt-1 text-xs text-[var(--color-muted)]">
              {tr(
                "fpkg.outputPathHint",
                undefined,
                "Relative paths resolve on the engine host and ~/ expands to its home folder. A missing output folder is created when conversion starts.",
              )}
            </div>
          </div>
        </div>
      </Card>

      {(error || jobError) && (
        <Callout tone="error" title={tr("fpkg.error", undefined, "Conversion error")}>
          {error || jobError}
        </Callout>
      )}

      {inspection && (
        <Card>
          <div className="flex flex-col gap-2 text-sm">
            <div className="font-medium">
              {inspection.title ?? inspection.content_id ?? inspection.source}
            </div>
            <div className="text-[var(--color-muted)]">{inspection.source}</div>
            <div className="flex flex-wrap gap-x-6 gap-y-1 text-[var(--color-text)]">
              <span>
                {tr("fpkg.files", undefined, "Files")}: {inspection.files}
              </span>
              <span>
                {tr("fpkg.size", undefined, "Game size")}: {prettyBytes(inspection.bytes)}
              </span>
              <span>
                {tr("fpkg.planned", undefined, "Package")}: {prettyBytes(inspection.planned_size)}
              </span>
              {inspection.output_free != null && (
                <span>
                  {tr("fpkg.free", undefined, "Free")}: {prettyBytes(inspection.output_free)}
                </span>
              )}
            </div>
            {inspection.output_free != null &&
              inspection.output_free < convertSpaceNeeded(inspection.planned_size) && (
                <div className="text-[var(--color-warn)]">
                  {tr(
                    "fpkg.lowSpace",
                    {
                      need: prettyBytes(convertSpaceNeeded(inspection.planned_size)),
                      free: prettyBytes(inspection.output_free),
                    },
                    "Low on free space: converting can need up to {need} on the output drive, which has {free}.",
                  )}
                </div>
              )}
            {inspection.content_id && (
              <div className="text-[var(--color-muted)]">
                {tr("fpkg.contentId", undefined, "Content id")}: {inspection.content_id}
              </div>
            )}
            {inspection.required_firmware && (
              <div className="text-[var(--color-muted)]">
                {tr("fpkg.firmware", undefined, "Requires firmware")}:{" "}
                {inspection.required_firmware}
              </div>
            )}
            {warnings.length > 0 && (
              <div className="mt-2 flex flex-col gap-1">
                {warnings.map((c) => (
                  <div key={c.name} className="text-[var(--color-warn)]">
                    {c.name}: {c.detail}
                  </div>
                ))}
              </div>
            )}
            {warnings.length === 0 && (
              <div className="mt-2 text-[var(--color-good)]">
                {tr("fpkg.ready", undefined, "Everything the package needs is here.")}
              </div>
            )}
            <details className="mt-1">
              <summary className="cursor-pointer text-[var(--color-muted)]">
                {passed.length} {tr("fpkg.checks", undefined, "checks passed")}
              </summary>
              <ul className="mt-1 flex flex-col gap-0.5 text-[var(--color-muted)]">
                {passed.map((c) => (
                  <li key={c.name}>
                    {c.name} — {c.detail}
                  </li>
                ))}
              </ul>
            </details>
          </div>
        </Card>
      )}

      {running && (
        <Card>
          <div className="flex flex-col gap-2">
            <ProgressBar
              value={job && job.total_bytes ? (job.bytes_sent ?? 0) / job.total_bytes : 0}
              tone="accent"
              label={
                kind === "ffpfsc"
                  ? tr("fpkg.compressing", undefined, "Compressing…")
                  : tr("fpkg.converting", undefined, "Converting…")
              }
            />
            <div className="text-sm text-[var(--color-muted)]">
              {prettyBytes(job?.bytes_sent ?? 0)} / {prettyBytes(job?.total_bytes ?? 0)}
            </div>
            <div>
              <Button
                variant="danger"
                onClick={() => {
                  void cancelConversion();
                }}
              >
                {tr("fpkg.cancel", undefined, "Cancel")}
              </Button>
            </div>
          </div>
        </Card>
      )}

      {job?.status === "failed" && (
        <Callout
          tone="error"
          title={
            kind === "ffpfsc"
              ? tr("fpkg.compressFailed", undefined, "The compression failed")
              : tr("fpkg.failed", undefined, "The conversion failed")
          }
        >
          {job.error}
        </Callout>
      )}

      {job?.status === "done" && kind === "ffpfsc" && (
        <Card>
          <div className="flex flex-col gap-2 text-sm">
            <div className="text-[var(--color-good)]">
              {tr("fpkg.compressed", undefined, "Compressed image written and verified")}
            </div>
            <div className="break-all text-[var(--color-text)]">{job.dest}</div>
            <div className="text-[var(--color-muted)]">
              {tr(
                "fpkg.compressedNext",
                undefined,
                "Upload it to the console like any other game image and mount it with ShadowMountPlus.",
              )}
            </div>
          </div>
        </Card>
      )}

      {job?.status === "done" && kind === "fpkg" && (
        <>
          <Card>
            <div className="flex flex-col gap-2 text-sm">
              <div className="text-[var(--color-good)]">
                {tr("fpkg.done", undefined, "Package written")}
              </div>
              <div className="break-all text-[var(--color-text)]">{job.dest}</div>
            </div>
          </Card>

          {/* Only this step needs the console — the conversion above ran on
              the machine hosting the engine, so it stays available with no
              PS5 connected. */}
          <ConnectionGate require="payload">
            <Card>
              <div className="flex flex-col gap-2 text-sm">
                <div className="flex gap-2">
                  <Button
                    onClick={() => void install()}
                    disabled={installing || !job.dest || !host?.trim()}
                  >
                    {installing
                      ? tr("fpkg.installing", undefined, "Installing…")
                      : tr("fpkg.install", undefined, "Install on the console")}
                  </Button>
                </div>
                {installResult && (
                  <div className="text-[var(--color-text)]">{installResult}</div>
                )}
              </div>
            </Card>
          </ConnectionGate>
        </>
      )}

      <div className="flex flex-col gap-1">
        <div className="text-sm font-medium">
          {tr("fpkg.compression", undefined, "Compression")}
        </div>
        <SegmentedControl
          ariaLabel={tr("fpkg.compression", undefined, "Compression")}
          value={compression}
          onChange={(v) => setCompression(v as FpkgCompression)}
          segments={[
            { value: "fast", label: tr("fpkg.compressionFast", undefined, "Fast") },
            { value: "balanced", label: tr("fpkg.compressionBalanced", undefined, "Balanced") },
            { value: "smallest", label: tr("fpkg.compressionSmallest", undefined, "Smallest") },
          ]}
        />
        <div className="text-xs text-[var(--color-muted)]">
          {compression === "fast"
            ? tr(
                "fpkg.compressionFastAbout",
                undefined,
                "About 4× quicker than Balanced; the package comes out roughly 5% larger.",
              )
            : compression === "smallest"
              ? tr(
                  "fpkg.compressionSmallestAbout",
                  undefined,
                  "About 1.5× slower than Balanced for a package under 1% smaller.",
                )
              : tr(
                  "fpkg.compressionBalancedAbout",
                  undefined,
                  "Within a few percent of Sony's own packages. Every block is checked against the source before it is kept.",
                )}
        </div>
      </div>

      <div className="flex flex-wrap gap-2">
        <Button
          onClick={() => void convert()}
          disabled={running || !source.trim() || (inspection !== null && inspection.files === 0)}
        >
          {tr("fpkg.convert", undefined, "Convert to FPKG")}
        </Button>
        {isImage && (
          <Button variant="secondary" onClick={() => void compress()} disabled={running}>
            {tr("fpkg.compress", undefined, "Compress to .ffpfsc")}
          </Button>
        )}
      </div>
      {isImage && (
        <div className="text-xs text-[var(--color-muted)]">
          {tr(
            "fpkg.compressAbout",
            undefined,
            "Compress to .ffpfsc keeps the game as a mountable image, 40–60% smaller: the console decompresses it as the game reads. It goes next to the source unless you choose an output folder, and it is read back and checked against the source before it is kept.",
          )}
        </div>
      )}
    </div>
  );
}
