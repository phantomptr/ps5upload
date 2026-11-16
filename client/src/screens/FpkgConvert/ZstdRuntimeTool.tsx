// A game in zstd packs is read through ampr-emu-ng in its fakelib. Releases before 0.3.4 hang
// some games at startup (ASTRO BOT), 0.4.0 refuses very large games, and drakmor's ampr_emu doesn't read zstd packs at all. This
// puts the newest ampr-emu-ng in where the game stands: a folder in place, an image on the PS5
// through a ShadowMount+ backport. On a PS5 it also says when ShadowMount+'s emulator updates
// would swap the game's runtime for one that won't run it.

import { useEffect, useState, useRef } from "react";
import { RefreshCw } from "lucide-react";

import {
  fpkg,
  type FpkgInspection,
  type SmpEmulatorCheck,
  type ZstdRuntimeOutcome,
} from "../../api/fpkg";
import { Button } from "../../components";
import { isRemotePath } from "../../lib/remotePath";
import { useTr, type Translator } from "../../state/lang";

const IMAGE = /\.(exfat|ffpkg|ufs2|ffpfs|ffpfsc)$/i;

/** Whether the button is offered for `source`: a zstd-packed game whose runtime needs replacing,
 *  in a folder (here or on the PS5) or an image on the PS5. */
export function zstdRuntimeOffered(
  source: string,
  inspection: FpkgInspection | null,
): boolean {
  const s = source.trim();
  const r = inspection?.zstd_runtime;
  if (!s || !(r?.outdated || r?.capped)) return false;
  const onConsole = s.startsWith("ps5://");
  if (!onConsole && isRemotePath(s)) return false;
  return onConsole || !IMAGE.test(s);
}

/** The console of a `ps5://host/…` source. */
export function consoleOf(source: string): string | null {
  if (!source.startsWith("ps5://")) return null;
  const rest = source.slice("ps5://".length);
  const host = rest.split("/")[0];
  return host || null;
}

/** What the game has now, in words. */
export function runtimeNow(
  inspection: FpkgInspection | null,
  tr: Translator,
): string | null {
  const r = inspection?.zstd_runtime;
  if (!r) return null;
  if (r.kind === "missing")
    return tr(
      "zstd_runtime_missing",
      undefined,
      "This game is in zstd packs and has no runtime to read them.",
    );
  if (r.kind === "ampr_emu")
    return tr(
      "zstd_runtime_drakmor",
      { version: r.version ?? "" },
      "This game is in zstd packs, but its emulator is ampr_emu {version}, which reads LZ4 packs only. It won't start like this.",
    );
  if (r.kind === "ampr-emu-ng" && r.outdated)
    return tr(
      "zstd_runtime_old",
      { version: r.version ?? "" },
      "This game's zstd runtime is ampr-emu-ng {version}. Versions before 0.3.4 can hang some games at startup (ASTRO BOT is one).",
    );
  if (r.kind === "ampr-emu-ng" && r.capped)
    return tr(
      "zstd_runtime_capped",
      { version: r.version ?? "" },
      "This game's zstd runtime is ampr-emu-ng {version}. Version 0.4.0 refuses very large games (\"index tables need … MiB, over the 48 MiB\"); 0.4.1 and newer start them.",
    );
  return null;
}

/** The warning for ShadowMount+'s emulator folder, or null when it doesn't touch zstd games. */
export function smpWarning(
  check: SmpEmulatorCheck | null,
  tr: Translator,
): string | null {
  if (!check?.breaks_zstd || !check.path) return null;
  const what =
    check.kind === "ampr_emu"
      ? `ampr_emu ${check.version ?? ""}`.trim()
      : check.kind === "ampr-emu-ng"
        ? `ampr-emu-ng ${check.version ?? ""}`.trim()
        : check.path;
  return tr(
    "zstd_runtime_smp_swaps",
    { path: check.path, what },
    "ShadowMount+ puts {path} ({what}) into every game in place of the game's own, and that one won't start games in zstd packs. In ShadowMount+'s settings, turn off emulator updates, or remove that file.",
  );
}

export function ZstdRuntimeTool({
  source,
  inspection,
  locked,
  onUpdated,
}: {
  source: string;
  inspection: FpkgInspection | null;
  /** A build is running: one job at a time on this screen. */
  locked: boolean;
  /** The game changed: check it again. */
  onUpdated: () => void;
}) {
  const tr = useTr();
  const [busy, setBusy] = useState(false);
  const shown = useRef(source);
  const [done, setDone] = useState<ZstdRuntimeOutcome | null>(null);
  const [failed, setFailed] = useState<string | null>(null);
  const [smp, setSmp] = useState<SmpEmulatorCheck | null>(null);
  const host = consoleOf(source.trim());
  const now = runtimeNow(inspection, tr);
  useEffect(() => {
    shown.current = source;
    setDone(null);
    setFailed(null);
  }, [source]);
  useEffect(() => {
    setSmp(null);
    if (!host) return;
    let live = true;
    fpkg
      .smpEmulator(host)
      .then((c) => {
        if (live) setSmp(c);
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [host]);
  const warning = smpWarning(smp, tr);
  return (
    <>
      <Button
        size="sm"
        variant="secondary"
        leftIcon={<RefreshCw size={14} />}
        data-testid="zstd-runtime-update"
        disabled={locked || busy || !!done}
        title={tr(
          "zstd_runtime_update_hint",
          undefined,
          "Puts the newest ampr-emu-ng into this game, checked against the digest GitHub publishes for it. A folder is changed in place. An image isn't changed: the runtime goes in a ShadowMount+ backport folder.",
        )}
        onClick={() => {
          const started = source;
          setBusy(true);
          setFailed(null);
          fpkg
            .updateZstdRuntime(started.trim())
            .then((outcome) => {
              // Another game was picked meanwhile: this answer is not about what is shown now.
              if (shown.current !== started) return;
              setDone(outcome);
              onUpdated();
            })
            .catch((e: unknown) => {
              if (shown.current === started) setFailed(e instanceof Error ? e.message : String(e));
            })
            .finally(() => setBusy(false));
        }}
      >
        {tr("zstd_runtime_update", undefined, "Update the zstd runtime")}
      </Button>
      {(now || done || failed || warning) && (
        <div
          className="w-full space-y-1 text-xs"
          data-testid="zstd-runtime-status"
          role="status"
        >
          {!done && now && <p className="text-[var(--color-warn)]">{now}</p>}
          {failed && <p className="text-[var(--color-bad)]">{failed}</p>}
          {done && (
            <p className="text-[var(--color-muted)]">
              {done.already
                ? tr(
                    "zstd_runtime_already",
                    { to: done.to },
                    "It already has {to}, the newest.",
                  )
                : done.kind === "backport"
                  ? tr(
                      "zstd_runtime_done_backport",
                      { from: done.from, to: done.to, folder: done.folder },
                      "{from} → {to}, as a ShadowMount+ backport in {folder}. The image isn't changed. Start the game to try it.",
                    )
                  : tr(
                      "zstd_runtime_done_folder",
                      { from: done.from, to: done.to },
                      "{from} → {to} in the game's folder. Start the game to try it.",
                    )}
            </p>
          )}
          {warning && <p className="text-[var(--color-warn)]">{warning}</p>}
        </div>
      )}
    </>
  );
}
