import { useState } from "react";
import { FileArchive, Square } from "lucide-react";

import { Button, Spinner } from "../../components";
import { ProgressBar } from "../../components/ProgressBar";
import { formatBytes } from "../../lib/format";
import { invoke } from "../../lib/invokeLogged";
import { startLinkDownload } from "../../lib/linkDownload";
import { pickPath } from "../../lib/pickPath";
import { useLinkInstallPrefs } from "../../state/linkInstallPrefs";
import {
  archiveInstallFor,
  chooseArchive,
  clearArchiveInstall,
  downloadArchiveParts,
  parseArchiveLinks,
  stopArchiveDownload,
  type ArchiveLinkDeps,
  runArchiveInstall,
  submitArchivePassword,
  useArchiveInstallStore,
} from "../../state/archiveInstall";
import { useTr } from "../../state/lang";
import { RarPasswordPrompt } from "../Upload/RarPasswordPrompt";

/** Downloads a link to this computer under the file's own name (parts of a set find each
 *  other by name), with the engine's ordinary link downloader. */
const linkDeps: ArchiveLinkDeps = {
  start: async (url, insecureTls) => {
    const r = await startLinkDownload({ url, insecureTls, destDir: null, keepName: true });
    if (r.kind === "started") return { download_id: r.id, path: r.path, total: r.total };
    // Already downloading (a second click, another tab): follow that download.
    if (r.kind === "attached") {
      const st = (await invoke("pkg_remote_download_status", { id: r.id })) as {
        path?: string;
        total?: number;
      };
      return { download_id: r.id, path: st.path, total: st.total };
    }
    throw new Error(`${r.message} Move or rename that file, then try again.`);
  },
  status: (id) => invoke("pkg_remote_download_status", { id }),
  cancel: (id) => invoke("pkg_remote_download_cancel", { id }),
  sleep: (ms) => new Promise((r) => setTimeout(r, ms)),
};

/** Example addresses, not words: nothing to translate. */
const LINKS_PLACEHOLDER =
  "https://example.com/game.part1.rar\nhttps://example.com/game.part2.rar";

/** Install the packages inside a ZIP, 7z or RAR (R6, #370): they are unpacked to the console
 *  over AVA1 (no extraction on this computer) and each is queued as its own install, base
 *  first. The run lives in a store (state/archiveInstall), so leaving this screen neither
 *  stops it nor forgets where it was. */
export function ArchivePackagesCard({ host }: { host: string }) {
  const tr = useTr();
  const st = useArchiveInstallStore((s) => archiveInstallFor(s, host));
  const busy = st?.busy ?? false;
  const phase = st?.phase ?? null;
  const dl = st?.downloading ?? null;
  const insecure = useLinkInstallPrefs((s) => s.insecureFor(host));
  const [linksText, setLinksText] = useState("");
  const links = parseArchiveLinks(linksText);

  async function choose() {
    const picked = await pickPath({
      mode: "file",
      title: tr("arcpkg.pick", undefined, "Choose an archive"),
      filters: [{ name: "ZIP, 7z, RAR", extensions: ["zip", "7z", "rar"] }],
    });
    if (picked) await chooseArchive(host, picked);
  }

  const phaseText = (() => {
    if (!phase) return null;
    switch (phase.phase) {
      case "listing":
        return tr("rarpkg.listing", undefined, "Reading the archive…");
      case "unpacking":
        return phase.total > 0
          ? tr(
              "rarpkg.unpacking",
              {
                sent: formatBytes(phase.sent),
                total: formatBytes(phase.total),
              },
              "Unpacking to the PS5: {sent} of {total}",
            )
          : tr("rarpkg.unpackingNoTotal", undefined, "Unpacking to the PS5…");
      case "reading":
        return tr(
          "rarpkg.reading",
          { n: phase.index, count: phase.count },
          "Checking package {n} of {count}…",
        );
      case "installing":
        return tr(
          "rarpkg.installing",
          { n: phase.index, count: phase.count, name: phase.name },
          "Installing {n} of {count}: {name}",
        );
    }
  })();
  const error =
    st?.error === "not_first"
      ? tr(
          "rarpkg.notFirst",
          undefined,
          "Choose the first part of the archive (the .rar or .part1.rar file).",
        )
      : st?.error === "not_archive"
        ? tr(
            "arcpkg.notArchive",
            undefined,
            "That file is not a .zip, .7z or .rar archive.",
          )
        : st?.error === "no_first_part"
          ? tr(
              "arcpkg.noFirstPart",
              undefined,
              "The files were downloaded, but none of them is the first part of an archive (.zip, .7z, .rar or .part1.rar). Check the links.",
            )
          : (st?.error ?? null);

  return (
    <section
      className="rounded-[var(--radius-panel)] border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] shadow-[var(--edge-highlight),var(--shadow-1)] p-4"
      data-testid="archive-packages-card"
    >
      <header className="mb-1 flex items-center gap-2">
        <FileArchive size={15} aria-hidden />
        <h3 className="text-sm font-semibold">
          {tr("arcpkg.title", undefined, "Install packages from an archive")}
        </h3>
      </header>
      <p className="mb-3 text-xs text-[var(--color-muted)]">
        {tr(
          "arcpkg.help",
          undefined,
          "For a .zip, .7z or .rar that holds one or more .pkg files, in any folder. Only the packages are unpacked, straight to the PS5 (nothing is extracted on this computer), then installed one by one: the base game before its update, then DLC. A multi-part RAR (.part1.rar, .part2.rar…) works: choose the first part and keep the rest beside it. A password is asked for when a RAR needs one; password-protected ZIP and 7z files are not supported.",
        )}
      </p>
      <div className="flex flex-wrap items-center gap-2">
        <Button
          variant="secondary"
          size="sm"
          onClick={choose}
          disabled={busy || !!dl}
        >
          {tr("arcpkg.choose", undefined, "Choose archive…")}
        </Button>
        {st?.archive && (
          <span
            className="min-w-0 truncate font-mono text-xs text-[var(--color-muted)]"
            title={st.archive}
          >
            {st.archive.replace(/\\/g, "/").split("/").pop()}
          </span>
        )}
        {st && !busy && !dl && (
          <Button
            variant="ghost"
            size="sm"
            onClick={() => clearArchiveInstall(host)}
          >
            {tr("clear", undefined, "Clear")}
          </Button>
        )}
      </div>
      {/* The archive is behind download links (often several, one per part of a split RAR):
          fetch the parts to this computer, then carry on as if the first had been chosen. */}
      <details className="rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface)] mt-3 text-xs">
        <summary className="cursor-pointer px-3 py-2 text-[var(--color-text)]">
          {tr(
            "arcpkg.links.title",
            undefined,
            "The archive is behind download links",
          )}
        </summary>
        <div className="px-3 pb-3">
          <p className="mb-2 text-[var(--color-muted)]">
            {tr(
              "arcpkg.links.help",
              undefined,
              "Paste the direct link to the archive, or to every part of a split one, one per line. The files are downloaded to this computer (Downloads/ps5upload), which needs room for all of them; then the packages inside are unpacked to the PS5 as above.",
            )}
          </p>
          <textarea
            value={linksText}
            onChange={(e) => setLinksText(e.currentTarget.value)}
            disabled={busy || !!dl}
            rows={3}
            placeholder={LINKS_PLACEHOLDER}
            className="input w-full! font-mono text-xs"
            data-testid="archive-links"
          />
          <div className="mt-2 flex flex-wrap items-center gap-2">
            {dl ? (
              <Button
                variant="secondary"
                size="sm"
                leftIcon={<Square size={12} />}
                onClick={() => stopArchiveDownload(host)}
              >
                {tr("stop", undefined, "Stop")}
              </Button>
            ) : (
              <Button
                variant="secondary"
                size="sm"
                disabled={busy || links.length === 0}
                onClick={() =>
                  void downloadArchiveParts(host, links, insecure, linkDeps)
                }
                data-testid="archive-links-go"
              >
                {tr(
                  "arcpkg.links.go",
                  { count: links.length },
                  "Download {count} file(s)",
                )}
              </Button>
            )}
          </div>
          {dl && (
            <div className="mt-2" role="status">
              <div className="mb-1 flex items-center justify-between">
                <span>
                  {tr(
                    "arcpkg.links.progress",
                    { n: dl.index, count: dl.count },
                    "Downloading file {n} of {count} to this computer…",
                  )}
                </span>
                {dl.total > 0 && (
                  <span className="tabular-nums text-[var(--color-muted)]">
                    {formatBytes(dl.written)} / {formatBytes(dl.total)}
                  </span>
                )}
              </div>
              <ProgressBar
                value={dl.total > 0 ? dl.written / dl.total : null}
                size="sm"
              />
            </div>
          )}
          {st && st.downloaded.length > 0 && !dl && (
            <p className="mt-2 text-[var(--color-muted)]">
              {tr(
                "arcpkg.links.kept",
                {
                  count: st.downloaded.length,
                  dir: st.downloaded[0].replace(/[\\/][^\\/]*$/, ""),
                },
                "{count} downloaded file(s) are in {dir}. They are kept so the install can be retried; delete them when you are done.",
              )}
            </p>
          )}
        </div>
      </details>
      {st?.inspecting && (
        <div className="mt-2 flex items-center gap-2 text-xs text-[var(--color-muted)]">
          <Spinner size={14} tone="accent" />
          {tr("rarpkg.listing", undefined, "Reading the archive…")}
        </div>
      )}
      {st?.passwordProblem && (
        <RarPasswordPrompt
          problem={st.passwordProblem}
          onSubmit={(pw) => void submitArchivePassword(host, pw)}
        />
      )}
      {st?.packages && st.packages.length > 0 && (
        <div className="mt-3">
          <ul className="rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface)] mb-2 max-h-40 overflow-auto text-xs">
            {st.packages.map((p) => (
              <li
                key={p.path}
                className="flex items-center gap-2 px-2.5 py-1.5"
              >
                <span
                  className="min-w-0 flex-1 truncate font-mono"
                  title={p.path}
                >
                  {p.path}
                </span>
                <span className="shrink-0 tabular-nums text-[var(--color-muted)]">
                  {formatBytes(p.size)}
                </span>
              </li>
            ))}
          </ul>
          <Button
            variant="primary"
            size="sm"
            onClick={() => void runArchiveInstall(host)}
            disabled={busy}
            data-testid="archive-packages-run"
          >
            {tr(
              "rarpkg.run",
              { count: st.packages.length },
              "Unpack and install {count} package(s)",
            )}
          </Button>
        </div>
      )}
      {st?.packages && st.packages.length === 0 && (
        <p className="mt-2 text-xs text-[var(--color-warn)]">
          {tr(
            "rarpkg.none",
            undefined,
            "That archive has no .pkg files in it.",
          )}
        </p>
      )}
      {busy && phaseText && (
        <div className="mt-3" role="status">
          <div className="mb-1 flex items-center gap-2 text-xs">
            <Spinner size={14} tone="accent" />
            <span>{phaseText}</span>
          </div>
          {phase?.phase === "unpacking" && phase.total > 0 && (
            <ProgressBar value={phase.sent / phase.total} size="sm" />
          )}
          {(phase?.phase === "reading" || phase?.phase === "installing") &&
            phase.count > 0 && (
              <ProgressBar value={(phase.index - 1) / phase.count} size="sm" />
            )}
          <p className="mt-1 text-[11px] text-[var(--color-muted)]">
            {tr(
              "arcpkg.keepsRunning",
              undefined,
              "This keeps running if you open another screen; it also shows in Tasks.",
            )}
          </p>
        </div>
      )}
      {st?.result && (
        <div className="mt-3 text-xs" role="status">
          <p
            className={
              st.result.ok
                ? "text-[var(--color-good)]"
                : "text-[var(--color-warn)]"
            }
          >
            {st.result.message}
          </p>
          <ul className="mt-1 space-y-0.5">
            {st.result.outcomes
              .filter((o) => o.status !== "installed")
              .map((o) => (
                <li key={o.entry} className="text-[var(--color-muted)]">
                  {o.entry}: {o.message}
                </li>
              ))}
          </ul>
        </div>
      )}
      {error && (
        <p role="alert" className="mt-2 text-xs text-[var(--color-bad)]">
          {error}
        </p>
      )}
    </section>
  );
}
