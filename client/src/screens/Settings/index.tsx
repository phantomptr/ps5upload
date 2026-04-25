import { useEffect, useState } from "react";
import {
  Settings as SettingsIcon,
  Moon as SleepIcon,
  Globe,
  Upload as UploadIcon,
  FileJson,
  Download,
  RefreshCw,
  CheckCircle2,
  AlertTriangle,
  Loader2,
} from "lucide-react";

import { PageHeader } from "../../components";

import { useKeepAwakeStore } from "../../state/keepAwake";
import { useLangStore, useTr, LANGUAGES } from "../../state/lang";
import { useUploadSettingsStore } from "../../state/uploadSettings";
import { userConfigPath } from "../../state/userConfig";
import { useUpdateStore, type UpdatePhase } from "../../state/update";
import type { LanguageCode } from "../../i18n";

function Section({
  title,
  children,
  /** When true, section spans both columns of the parent grid. Used
   *  for settings whose body is tall enough (warning card, long
   *  description) that side-by-side layout would look cramped. */
  full = false,
}: {
  title: string;
  children: React.ReactNode;
  full?: boolean;
}) {
  return (
    <section
      className={
        "rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4 " +
        (full ? "md:col-span-2 xl:col-span-3" : "")
      }
    >
      <h2 className="mb-3 text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
        {title}
      </h2>
      {children}
    </section>
  );
}

export default function SettingsScreen() {
  const { enabled, supported, lastError, setEnabled, syncFromBackend } =
    useKeepAwakeStore();
  const lang = useLangStore((s) => s.lang);
  const setLang = useLangStore((s) => s.setLang);
  const tr = useTr();
  const {
    alwaysOverwrite,
    showTransferFiles,
    setAlwaysOverwrite,
    setShowTransferFiles,
  } = useUploadSettingsStore();

  const [cfgPath, setCfgPath] = useState<string | null>(null);

  useEffect(() => {
    syncFromBackend();
    // Resolve the settings-mirror path so the Settings → Storage card
    // can show the user where their settings.json lives. One-shot per
    // mount — the path is stable once the OS tells us the home dir.
    userConfigPath().then(setCfgPath).catch(() => setCfgPath(null));
  }, [syncFromBackend]);

  return (
    <div className="p-6">
      <PageHeader
        icon={SettingsIcon}
        title={tr("settings", undefined, "Settings")}
        description={tr(
          "settings_description",
          undefined,
          "App preferences — language, keep-awake, upload defaults, and where your settings are stored.",
        )}
      />

      {/* Responsive grid — 2 columns at md, 3 at xl. Short/simple
          settings pack in; cards with warnings or long descriptions
          mark themselves `full` and take all columns. */}
      <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
        <Section title={tr("language", undefined, "Language")}>
          <label className="flex items-center gap-3 text-sm">
            <Globe size={14} className="text-[var(--color-muted)]" />
            <select
              value={lang}
              onChange={(e) => setLang(e.target.value as LanguageCode)}
              className="flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-1.5 text-sm"
            >
              {LANGUAGES.map((l) => (
                <option key={l.code} value={l.code}>
                  {l.label}
                </option>
              ))}
            </select>
          </label>
        </Section>

        <Section title={tr("keep_awake", undefined, "Keep Awake")}>
          <label className="flex items-start gap-3 text-sm">
            <input
              type="checkbox"
              checked={enabled}
              disabled={!supported}
              onChange={(e) => setEnabled(e.target.checked)}
              className="mt-0.5 accent-[var(--color-accent)]"
            />
            <div>
              <div className="flex items-center gap-2 font-medium">
                <SleepIcon size={14} />
                {tr("keep_awake_on", undefined, "Keep the computer awake")}
              </div>
              <div className="mt-0.5 text-xs text-[var(--color-muted)]">
                {tr(
                  "keep_awake_hint",
                  undefined,
                  "Blocks display and system sleep while the app is running so long uploads don't get interrupted.",
                )}
              </div>
              {!supported && (
                <div className="mt-1 text-xs text-[var(--color-warn)]">
                  {tr(
                    "keep_awake_unsupported",
                    undefined,
                    "Not yet supported on this platform.",
                  )}
                </div>
              )}
              {lastError && (
                <div className="mt-1 text-xs text-[var(--color-bad)]">
                  {lastError}
                </div>
              )}
            </div>
          </label>
        </Section>

        <Section title={tr("upload", undefined, "Upload")}>
          <div className="grid gap-3">
            <label className="flex items-start gap-3 text-sm">
              <input
                type="checkbox"
                checked={alwaysOverwrite}
                onChange={(e) => setAlwaysOverwrite(e.target.checked)}
                className="mt-0.5 accent-[var(--color-accent)]"
              />
              <div>
                <div className="flex items-center gap-2 font-medium">
                  <UploadIcon size={14} />
                  {tr("always_overwrite", undefined, "Always overwrite without asking")}
                </div>
                <div className="mt-0.5 text-xs text-[var(--color-muted)]">
                  {tr(
                    "always_overwrite_hint",
                    undefined,
                    "Skip the confirmation dialog when a destination already has files. Leave off to see the Override / Resume / Cancel prompt.",
                  )}
                </div>
              </div>
            </label>

            <label className="flex items-start gap-3 text-sm">
              <input
                type="checkbox"
                checked={showTransferFiles}
                onChange={(e) => setShowTransferFiles(e.target.checked)}
                className="mt-0.5 accent-[var(--color-accent)]"
              />
              <div>
                <div className="font-medium">
                  {tr("show_file_list", undefined, "Show file list during transfer")}
                </div>
                <div className="mt-0.5 text-xs text-[var(--color-muted)]">
                  {tr(
                    "show_file_list_hint",
                    undefined,
                    "Display the scrollable list of files being transferred beneath the progress bar. Turn off if the list feels noisy on folders with thousands of files — you'll still see overall progress, speed, and ETA.",
                  )}
                </div>
              </div>
            </label>
          </div>
        </Section>

        {/* Updates takes the full row — the release-notes blob can
            be multiple paragraphs and deserves reading space. The
            "updates" i18n key already exists across every locale in
            i18n.ts; inner strings fall back to English for now. */}
        <Section title={tr("updates", undefined, "Updates")} full>
          <UpdatesPanel />
        </Section>

        {/* Storage is the long row — path text is wide, so it takes
            the whole grid width. Keeps it visually distinct as "info,
            not settings" — you can't change where ps5upload writes
            settings.json from the UI. */}
        <Section title={tr("storage", undefined, "Storage")} full>
          <div className="flex items-start gap-3 text-sm">
            <FileJson
              size={14}
              className="mt-0.5 shrink-0 text-[var(--color-muted)]"
            />
            <div className="min-w-0 flex-1">
              <div className="font-medium">{tr("settings_file", undefined, "Settings file")}</div>
              <div className="mt-0.5 text-xs text-[var(--color-muted)]">
                {tr(
                  "settings_file_hint",
                  undefined,
                  "Your preferences live in this JSON file — safe to back up, copy to another machine, or edit by hand (edits take effect next time you launch the app).",
                )}
              </div>
              <div className="mt-2 truncate rounded-md bg-[var(--color-surface)] px-2 py-1 font-mono text-xs">
                {cfgPath ?? tr("resolving", undefined, "resolving…")}
              </div>
            </div>
          </div>
        </Section>
      </div>
    </div>
  );
}

/** Renders the update state + controls. Pulls straight from
 *  `useUpdateStore` so the same sidebar badge that indicates
 *  "available" also drives this card's main CTA. */
function UpdatesPanel() {
  const tr = useTr();
  const phase = useUpdateStore((s) => s.phase);
  const checkNow = useUpdateStore((s) => s.checkNow);
  const download = useUpdateStore((s) => s.download);
  const dismissDownload = useUpdateStore((s) => s.dismissDownload);

  const [busy, setBusy] = useState(false);

  const handleCheck = async () => {
    if (busy) return;
    setBusy(true);
    try {
      await checkNow();
    } finally {
      setBusy(false);
    }
  };

  const handleDownload = async () => {
    if (busy) return;
    setBusy(true);
    try {
      await download();
    } finally {
      setBusy(false);
    }
  };

  const resultForNotes =
    phase.kind === "available" ||
    phase.kind === "downloading" ||
    phase.kind === "downloaded" ||
    phase.kind === "download-failed"
      ? phase.result
      : null;

  const canDownload =
    phase.kind === "available" || phase.kind === "download-failed";
  const hasPlatformBundle =
    resultForNotes?.download_url &&
    resultForNotes.download_url.length > 0;

  return (
    <div className="flex flex-col gap-3">
      <StatusRow phase={phase} />
      {resultForNotes && (
        <ReleaseNotes
          version={resultForNotes.latest_version}
          notes={resultForNotes.notes}
          pubDate={resultForNotes.pub_date}
        />
      )}
      {phase.kind === "downloaded" && (
        <div className="rounded-md border border-[var(--color-good)] bg-[var(--color-surface)] p-3 text-xs">
          <div className="mb-1 font-medium text-[var(--color-good)]">
            Downloaded. Finish installing:
          </div>
          <div className="mb-2 font-mono text-[var(--color-text)]">
            {phase.download.path}
          </div>
          <ol className="list-inside list-decimal space-y-0.5 text-[var(--color-muted)]">
            <li>Quit PS5Upload.</li>
            <li>
              {pickInstallHint(phase.download.path)}
            </li>
            <li>Launch the new version.</li>
          </ol>
          <button
            type="button"
            onClick={dismissDownload}
            className="mt-2 text-[10px] underline-offset-2 hover:underline"
          >
            Dismiss
          </button>
        </div>
      )}
      <div className="flex flex-wrap items-center gap-2">
        <button
          type="button"
          onClick={handleCheck}
          disabled={busy || phase.kind === "downloading"}
          className="inline-flex items-center gap-1.5 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-1.5 text-xs hover:bg-[var(--color-surface-3)] disabled:opacity-50"
        >
          <RefreshCw size={12} className={busy ? "animate-spin" : ""} />
          {tr("check_updates", undefined, "Check for updates")}
        </button>
        {canDownload && hasPlatformBundle && (
          <button
            type="button"
            onClick={handleDownload}
            disabled={busy}
            className="inline-flex items-center gap-1.5 rounded-md bg-[var(--color-accent)] px-3 py-1.5 text-xs font-medium text-[var(--color-accent-contrast)] hover:opacity-90 disabled:opacity-50"
          >
            <Download size={12} />
            {tr(
              "update_download_button",
              { version: resultForNotes?.latest_version ?? "" },
              `Download v${resultForNotes?.latest_version ?? ""}`,
            )}
          </button>
        )}
        {canDownload && !hasPlatformBundle && (
          <span className="text-xs text-[var(--color-muted)]">
            {tr(
              "update_no_platform_bundle",
              undefined,
              "No download for your platform in this release.",
            )}
          </span>
        )}
      </div>
    </div>
  );
}

/** Per-platform hint about how to install the downloaded file.
 *  Branch on the filename extension — more reliable than reading
 *  `navigator.userAgent` at runtime. */
function pickInstallHint(downloadPath: string): string {
  const lower = downloadPath.toLowerCase();
  if (lower.endsWith(".dmg")) {
    return "Double-click the .dmg, drag PS5Upload into Applications, replace when asked.";
  }
  if (lower.endsWith(".zip")) {
    return "Extract the zip and replace the existing PS5Upload with the new one.";
  }
  return "Replace the existing PS5Upload with the newly downloaded file.";
}

/** One-line summary of the current phase — the line the sidebar badge
 *  corresponds to. Kept minimal so the eye lands on the CTA below it. */
function StatusRow({ phase }: { phase: UpdatePhase }) {
  if (phase.kind === "idle") {
    return (
      <div className="text-xs text-[var(--color-muted)]">
        Haven't checked yet — the app checks once per launch in the
        background.
      </div>
    );
  }
  if (phase.kind === "checking") {
    return (
      <div className="inline-flex items-center gap-2 text-xs text-[var(--color-muted)]">
        <Loader2 size={12} className="animate-spin" />
        Checking GitHub releases…
      </div>
    );
  }
  if (phase.kind === "error") {
    return (
      <div className="inline-flex items-start gap-2 text-xs text-[var(--color-bad)]">
        <AlertTriangle size={12} className="mt-0.5 shrink-0" />
        <span>Couldn't check for updates: {phase.message}</span>
      </div>
    );
  }
  if (phase.kind === "up-to-date") {
    return (
      <div className="inline-flex items-center gap-2 text-xs text-[var(--color-good)]">
        <CheckCircle2 size={12} />
        You're on the latest (v{phase.result.current_version}).
      </div>
    );
  }
  if (phase.kind === "available") {
    return (
      <div className="inline-flex items-center gap-2 text-xs text-[var(--color-accent)]">
        <Download size={12} />
        v{phase.result.latest_version} is available — you're on v
        {phase.result.current_version}.
      </div>
    );
  }
  if (phase.kind === "downloading") {
    return (
      <div className="inline-flex items-center gap-2 text-xs text-[var(--color-accent)]">
        <Loader2 size={12} className="animate-spin" />
        Downloading v{phase.result.latest_version}…
      </div>
    );
  }
  if (phase.kind === "downloaded") {
    return (
      <div className="inline-flex items-center gap-2 text-xs text-[var(--color-good)]">
        <CheckCircle2 size={12} />
        v{phase.result.latest_version} downloaded — installer opened.
      </div>
    );
  }
  // download-failed
  return (
    <div className="inline-flex items-start gap-2 text-xs text-[var(--color-bad)]">
      <AlertTriangle size={12} className="mt-0.5 shrink-0" />
      <span>Download failed: {phase.message}</span>
    </div>
  );
}

function ReleaseNotes({
  version,
  notes,
  pubDate,
}: {
  version: string;
  notes: string;
  pubDate: string;
}) {
  if (!notes.trim()) return null;
  return (
    <div className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-3">
      <div className="mb-1 flex items-center justify-between text-[10px] uppercase tracking-wide text-[var(--color-muted)]">
        <span>What's new in v{version}</span>
        {pubDate && (
          <span className="font-mono">
            {pubDate.slice(0, 10) /* YYYY-MM-DD */}
          </span>
        )}
      </div>
      <pre className="max-h-48 overflow-auto whitespace-pre-wrap font-sans text-xs leading-relaxed text-[var(--color-text)]">
        {notes}
      </pre>
    </div>
  );
}

/* ModeCard removed along with the Fast/Safe resume picker. If we
 * reintroduce the choice later, restore from the commit that added
 * register.c (user rejected Safe mode because it hashed every remote
 * file and crashed the payload on large libraries). */
