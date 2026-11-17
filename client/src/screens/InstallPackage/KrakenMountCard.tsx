import { useEffect, useRef, useState, type ReactNode } from "react";
import { HardDrive } from "lucide-react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";

import { Button } from "../../components";
import { krakenIsTheMounter, krakenRuns } from "../../lib/mounter";
import { isMobile } from "../../lib/platform";
import { pickPath } from "../../lib/pickPath";
import { isTauriEnv } from "../../lib/tauriEnv";
import { useTr } from "../../state/lang";
import { placePkgForMount, type MountNameClash } from "../../state/pkgMount";
import { useConflictAsk } from "../FileSystem/ConflictAsk";
import { KrakenUpdateNote } from "./KrakenUpdateNote";
import { useSmpState } from "../GameHub/SmpNeeded";

/** Asks Replace / Keep both (Skip and Cancel stop) when the scan folder already holds the name,
 *  with the same question a copy onto the PS5 asks. Render `dialog`. */
export function useMountClashAsk(host: string): {
  onExists: (name: string, existingDir: boolean) => Promise<MountNameClash>;
  dialog: ReactNode;
} {
  const { ask, dialog } = useConflictAsk(host);
  const onExists = async (name: string, existingDir: boolean): Promise<MountNameClash> => {
    const d = await ask({ name, pickedDir: false, existingDir, canReplace: !existingDir, left: 0 });
    return d?.choice === "replace" ? "replace" : d?.choice === "keep" ? "keep" : null;
  };
  return { onExists, dialog };
}

/**
 * Shown on Install Package when Kraken Mount runs on the console: a PS5 package made with the
 * debug key can be put where Kraken Mount mounts it (a scan folder of the package drive), which
 * plays it without installing it. The package is never installed on this path.
 */
export function KrakenMountCard({ host }: { host: string }) {
  const tr = useTr();
  const { status } = useSmpState(host);
  const snapshot = status === "checking" ? null : status;
  const [busy, setBusy] = useState(false);
  const [results, setResults] = useState<{ name: string; ok: boolean; message: string }[]>([]);
  const clash = useMountClashAsk(host);
  // Results are about one console: another one starts with none, and a pick still going for the
  // old one adds no more.
  const hostRef = useRef(host);
  // Each pick's token: a console switch starts a new one, so the old pick's end can't clear
  // the new console's busy state, and the new console starts not busy.
  const pickRef = useRef(0);
  useEffect(() => {
    hostRef.current = host;
    pickRef.current++;
    setResults([]);
    setBusy(false);
  }, [host]);
  if (!krakenRuns(snapshot)) return null;
  const alone = krakenIsTheMounter(snapshot);

  const pick = async () => {
    const token = ++pickRef.current;
    const live = () => pickRef.current === token && hostRef.current === host;
    setBusy(true);
    setResults([]);
    const filters = [{ name: tr("install.kraken.filter", undefined, "PS5 package"), extensions: ["pkg"] }];
    try {
      const sel = isMobile()
        ? await pickPath({ mode: "file", filters })
        : await openDialog({ multiple: true, filters });
      const paths = (Array.isArray(sel) ? sel : sel ? [sel] : []) as string[];
      // One at a time: each waits for its upload, so the results line up with the picks.
      for (const p of paths) {
        if (!live()) break;
        const r = await placePkgForMount(p, host, { onExists: clash.onExists });
        if (!live()) break;
        const name = p.split(/[\\/]/).pop() ?? p;
        setResults((prev) => [...prev, { name, ok: r.ok, message: r.message }]);
      }
    } catch (e) {
      // Keep what the earlier files got.
      if (live())
        setResults((prev) => [...prev, { name: "", ok: false, message: e instanceof Error ? e.message : String(e) }]);
    } finally {
      if (pickRef.current === token) setBusy(false);
    }
  };

  return (
    <section
      className="mb-4 rounded-[var(--radius-panel)] border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] p-4 shadow-[var(--edge-highlight),var(--shadow-1)]"
      data-testid="kraken-mount-card"
    >
      <header className="mb-1 flex items-center gap-2">
        <HardDrive size={15} aria-hidden />
        <h3 className="text-sm font-semibold">
          {tr("install.kraken.title", undefined, "Kraken Mount is running on this PS5")}
        </h3>
      </header>
      <p className="mb-3 text-xs text-[var(--color-muted)]">
        {alone
          ? tr(
              "install.kraken.body",
              undefined,
              "A PS5 package made with the debug key (every package Convert makes is one) can be mounted instead of installed: PS5Upload copies it into the homebrew folder of the package drive, and Kraken Mount puts it on the home screen. Nothing is installed, and removing it later deletes just that file. PS4 packages, retail packages, updates and add-ons still install.",
            )
          : tr(
              "install.kraken.both",
              undefined,
              "ShadowMount+ is running too. The two must not run together: restart the PS5 with only one of them before mounting a package.",
            )}
      </p>
      <KrakenUpdateNote kraken={snapshot?.kraken} />
      {alone && isTauriEnv() && (
        <Button
          variant="secondary"
          size="sm"
          leftIcon={<HardDrive size={14} />}
          loading={busy}
          disabled={busy || !host.trim()}
          onClick={() => void pick()}
        >
          {tr("install.kraken.pick", undefined, "Put a package on the PS5 to mount")}
        </Button>
      )}
      {results.length > 0 && (
        <ul className="mt-3 flex flex-col gap-1 text-xs" data-testid="kraken-mount-results">
          {results.map((r, i) => (
            <li
              key={`${i}-${r.name}`}
              className={r.ok ? "text-[var(--color-good)]" : "text-[var(--color-bad)]"}
            >
              {r.name ? `${r.name}: ` : ""}
              {r.message}
            </li>
          ))}
        </ul>
      )}
      {clash.dialog}
    </section>
  );
}
