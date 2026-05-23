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
  Sun,
  Moon,
  MoonStar,
  Gauge,
} from "lucide-react";
import { useThemeStore, type Theme } from "../../state/theme";

import { PageHeader } from "../../components";

import { useKeepAwakeStore } from "../../state/keepAwake";
import { useLangStore, useTr, LANGUAGES } from "../../state/lang";
import { useUploadSettingsStore } from "../../state/uploadSettings";
import { userConfigPath } from "../../state/userConfig";
import { useUpdateStore, type UpdatePhase } from "../../state/update";
import type { LanguageCode } from "../../i18n";
// Static imports for stores already pulled into the main bundle by
// AppShell / Connection — the prior dynamic `import()` form here
// produced "dynamic import will not move module into another chunk"
// warnings at build because the modules were always loaded already.
// Keeping the imports static eliminates the warning + avoids the
// per-call promise overhead.
import { useScheduleStore } from "../../state/schedules";
import {
  getNotifPruneDays,
  setNotifPruneDays,
} from "../../state/notifications";
// useAuditLogStore + AuditEntry moved to screens/AuditLog/index.tsx
// (2.12.0 — promoted out of Settings junk drawer).

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

        <Section title={tr("settings_section_appearance", undefined, "Appearance")}>
          <ThemePicker />
        </Section>

        <Section title={tr("settings_section_bandwidth", undefined, "Bandwidth")}>
          <BandwidthHelp />
        </Section>

        <Section title={tr("settings_section_diagnostic", undefined, "Diagnostics")} full>
          <BugReportButton />
        </Section>

        <Section title={tr("settings_section_backup", undefined, "Backup / restore")} full>
          <BackupRestorePanel />
        </Section>

        {/* 2.12.0: Audit log promoted out of Settings to its own
            top-level route (/audit-log) under Diagnostics. It's a
            permanent local safety record, conceptually adjacent to
            Activity/Logs — burying it as a Settings card meant
            users who wanted "what did I delete last week?" had to
            scroll through preferences to find it. AuditLogPanel
            removed too. */}
        <Section title={tr("settings_section_schedules", undefined, "Schedules")} full>
          <SchedulesPanel />
        </Section>

        <Section title={tr("settings_section_notifications", undefined, "Notifications")} full>
          <NotifPrunePanel />
        </Section>
      </div>
    </div>
  );
}

/** Scheduler UI. Add/remove daily/weekly/once schedules that fire
 *  while the app is open. Limited to power_tick (keep PS5 awake) and
 *  notif (reminders) actions for now — extending to "trigger this
 *  upload at 3am" would need queue plumbing. */
function SchedulesPanel() {
  const tr = useTr();
  // Subscribe directly via the hook — selecting `s.schedules` keeps
  // the panel in sync without the prior dynamic-import dance.
  const schedules = useScheduleStore((s) => s.schedules);
  const [labelDraft, setLabelDraft] = useState("");
  const [hhmmDraft, setHhmmDraft] = useState("03:00");
  const [actionDraft, setActionDraft] = useState<"power_tick" | "notif">("notif");

  function add() {
    if (!labelDraft.trim() || !/^\d\d:\d\d$/.test(hhmmDraft)) return;
    useScheduleStore.getState().add({
      enabled: true,
      kind: "daily",
      action: actionDraft,
      hhmm: hhmmDraft,
      label: labelDraft.trim(),
    });
    setLabelDraft("");
  }

  function toggle(id: string, enabled: boolean) {
    useScheduleStore.getState().update(id, { enabled });
  }

  function remove(id: string) {
    useScheduleStore.getState().remove(id);
  }

  return (
    <div className="space-y-3 text-sm">
      <p className="text-[11px] text-[var(--color-muted)]">
        {tr(
          "schedules_caveat",
          undefined,
          "Schedules fire only while ps5upload is open. For true overnight automation, set a system cron job that hits the engine HTTP API instead.",
        )}
      </p>
      {schedules.length > 0 && (
        <ul className="space-y-1">
          {schedules.map((s) => (
            <li
              key={s.id}
              className="flex items-center gap-2 rounded-md border border-[var(--color-border)] p-2 text-xs"
            >
              <input
                type="checkbox"
                checked={s.enabled}
                onChange={(e) => toggle(s.id, e.target.checked)}
              />
              <span className="font-medium">{s.label}</span>
              <span className="text-[var(--color-muted)]">
                {s.kind === "daily" && `daily at ${s.hhmm}`}
                {s.kind === "weekly" && `weekly at ${s.hhmm}`}
                {s.kind === "once" && s.oneShotMs &&
                  `once at ${new Date(s.oneShotMs).toLocaleString()}`}
                {" · "}
                {s.action}
              </span>
              <button
                type="button"
                onClick={() => remove(s.id)}
                className="ml-auto text-[10px] text-[var(--color-muted)] hover:text-[var(--color-bad)]"
              >
                {tr("schedules_remove", undefined, "remove")}
              </button>
            </li>
          ))}
        </ul>
      )}
      <div className="flex flex-wrap items-center gap-2 rounded-md border border-dashed border-[var(--color-border)] p-2 text-xs">
        <input
          type="text"
          value={labelDraft}
          onChange={(e) => setLabelDraft(e.target.value)}
          placeholder={tr(
            "settings_schedule_label_placeholder",
            "Label (e.g. nightly tick)",
          )}
          className="flex-1 rounded border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 text-xs"
        />
        <input
          type="time"
          value={hhmmDraft}
          onChange={(e) => setHhmmDraft(e.target.value)}
          className="rounded border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 text-xs"
        />
        <select
          value={actionDraft}
          onChange={(e) => setActionDraft(e.target.value as "notif" | "power_tick")}
          className="rounded border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 text-xs"
        >
          <option value="notif">{tr("settings_notify_only", "Notify only")}</option>
          <option value="power_tick">
            {tr("settings_ps5_power_tick", "PS5 power tick")}
          </option>
        </select>
        <button
          type="button"
          onClick={add}
          disabled={!labelDraft.trim() || !/^\d\d:\d\d$/.test(hhmmDraft)}
          className="rounded-md bg-[var(--color-accent)] px-2 py-1 text-xs text-[var(--color-accent-contrast)] disabled:opacity-50"
        >
          {tr("schedules_add", undefined, "Add daily")}
        </button>
      </div>
    </div>
  );
}

/** Lets the user choose how long notifications are kept. Default 30
 *  days; 0 disables auto-prune. Pruning runs at app start + every 6h
 *  (wired in AppShell). Setting changes take effect on the next tick
 *  — instant pruning isn't worth the surprise of "I changed it to 1
 *  day and lost everything." */
function NotifPrunePanel() {
  const tr = useTr();
  const [days, setDays] = useState<number>(() => getNotifPruneDays());

  function pickDays(d: number) {
    setNotifPruneDays(d);
    setDays(d);
  }

  const options: Array<{ days: number; label: string }> = [
    { days: 7, label: "7 days" },
    { days: 30, label: "30 days" },
    { days: 90, label: "90 days" },
    { days: 0, label: "Never (keep all)" },
  ];

  return (
    <div className="space-y-2 text-sm">
      <div className="text-[11px] text-[var(--color-muted)]">
        {tr(
          "notif_prune_hint",
          undefined,
          "Notifications older than this are auto-deleted. Runs at startup and every 6 hours.",
        )}
      </div>
      <div className="flex flex-wrap gap-2">
        {options.map((o) => (
          <button
            key={o.days}
            type="button"
            onClick={() => pickDays(o.days)}
            className={`rounded-md border px-2 py-1 text-xs ${
              days === o.days
                ? "border-[var(--color-accent)] bg-[var(--color-accent-soft)] text-[var(--color-accent)]"
                : "border-[var(--color-border)] hover:bg-[var(--color-surface-3)]"
            }`}
          >
            {o.label}
          </button>
        ))}
      </div>
    </div>
  );
}

// 2.12.0: AuditLogPanel moved to its own top-level screen at
// /audit-log (under Diagnostics in the sidebar). See
// screens/AuditLog/index.tsx — the table render is identical;
// only the rehousing changed.

/**
 * Settings backup/restore. Bundles all the localStorage keys that
 * encode user preferences (theme, lang, roster, notifications,
 * activity history, upload settings) plus the Tauri-managed
 * settings.json into one JSON document. User can save it to disk
 * and re-import to migrate to a new machine or roll back after a
 * config corruption.
 *
 * Note: the on-disk settings.json (~/.ps5upload/settings.json) is
 * authoritative for the cross-app fields it covers; on import we
 * write back to localStorage and let the next launch's userConfig
 * sync flush to disk.
 */
function BackupRestorePanel() {
  const tr = useTr();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [info, setInfo] = useState<string | null>(null);

  // List every persisted key the renderer owns. Adding a new
  // store with localStorage backing? Add the key here so Backup
  // captures it. Off-by-one names like "activityHistory" (no -v1
  // suffix) are real — match what each store actually writes.
  const KEYS = [
    "ps5upload.theme",
    "ps5upload.host",
    "ps5upload.lang",
    "ps5upload.roster.v1",
    "ps5upload.notifications.v1",
    "ps5upload.activityHistory",
    "ps5upload.audit-log.v1",
    "ps5upload.upload-settings",
    "ps5upload.companion-suggestion.dismissed",
    "ps5upload.schedules.v1",
    "ps5upload.saved_searches.v1",
    "ps5upload.recent_paths.v1",
    "ps5upload.play_time.v1",
    "ps5upload.window_state.v1",
    "ps5upload.notif_prune_days.v1",
    "ps5upload.last_route",
    "ps5upload.fs.lastPath",
    "ps5upload.library.sort",
    "ps5upload.titleinfo.cache",
    "ps5upload.mount.lastDest",
    "ps5upload.install_queue.v1",
    "ps5upload.bandwidth_cap_mbps",
    "ps5upload.keep_awake",
    "ps5upload.always_overwrite",
    "ps5upload.show_transfer_files",
  ];

  async function exportBundle() {
    setBusy(true);
    setError(null);
    setInfo(null);
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const { writeTextFileToPath } = await import("../../lib/saveTextFile");
      const bundle: Record<string, unknown> = {
        meta: {
          generated_at_ms: Date.now(),
          schema: 1,
        },
        local_storage: {} as Record<string, string | null>,
      };
      const ls = bundle.local_storage as Record<string, string | null>;
      for (const k of KEYS) {
        ls[k] = window.localStorage.getItem(k);
      }
      const dest = await save({
        defaultPath: `ps5upload-settings-${Date.now()}.json`,
        filters: [{ name: "JSON", extensions: ["json"] }],
      });
      if (!dest || typeof dest !== "string") return;
      await writeTextFileToPath(dest, JSON.stringify(bundle, null, 2));
      setInfo(`Saved to ${dest}`);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  async function importBundle() {
    setBusy(true);
    setError(null);
    setInfo(null);
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const { readTextFileFromPath } = await import("../../lib/saveTextFile");
      const src = await open({
        multiple: false,
        filters: [{ name: "JSON", extensions: ["json"] }],
      });
      if (!src || typeof src !== "string") return;
      const text = await readTextFileFromPath(src);
      const parsed = JSON.parse(text);
      if (
        !parsed ||
        typeof parsed !== "object" ||
        typeof parsed.local_storage !== "object"
      ) {
        throw new Error("file is not a ps5upload settings bundle");
      }
      const ls = parsed.local_storage as Record<string, unknown>;
      const actions: Array<{ key: string; value: string | null }> = [];
      for (const k of KEYS) {
        const v = ls[k];
        if (typeof v === "string") {
          actions.push({ key: k, value: v });
        } else if (v === null) {
          actions.push({ key: k, value: null });
        } else if (v !== undefined) {
          throw new Error(`invalid value for ${k}`);
        }
      }
      for (const { key, value } of actions) {
        if (value === null) window.localStorage.removeItem(key);
        else window.localStorage.setItem(key, value);
      }
      const restored = actions.length;
      setInfo(
        `Restored ${restored} key${restored === 1 ? "" : "s"}. Restart the app for all changes to take effect.`,
      );
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="space-y-3 text-sm">
      <p className="text-xs text-[var(--color-muted)]">
        {tr(
          "settings_backup_hint",
          undefined,
          "Bundle theme, PS5 roster, notification history, activity log, and other preferences into one JSON file. Useful for moving to a new machine or recovering after wiping the app's storage.",
        )}
      </p>
      <div className="flex flex-wrap items-center gap-2">
        <button
          type="button"
          onClick={exportBundle}
          disabled={busy}
          className="inline-flex items-center gap-1 rounded-md bg-[var(--color-accent)] px-3 py-1.5 text-xs text-[var(--color-accent-contrast)] disabled:opacity-50"
        >
          {busy ? (
            <Loader2 size={11} className="animate-spin" />
          ) : (
            <Download size={11} />
          )}
          {tr("settings_backup_export", undefined, "Export bundle")}
        </button>
        <button
          type="button"
          onClick={importBundle}
          disabled={busy}
          className="inline-flex items-center gap-1 rounded-md border border-[var(--color-border)] px-3 py-1.5 text-xs hover:bg-[var(--color-surface)] disabled:opacity-50"
        >
          {tr("settings_backup_import", undefined, "Import bundle…")}
        </button>
      </div>
      {info && (
        <div className="flex items-center gap-1 text-[11px] text-[var(--color-good)]">
          <CheckCircle2 size={11} />
          {info}
        </div>
      )}
      {error && (
        <div className="flex items-start gap-1 text-[11px] text-[var(--color-bad)]">
          <AlertTriangle size={11} className="mt-0.5 shrink-0" />
          {error}
        </div>
      )}
    </div>
  );
}

/** Three-way theme picker. The sidebar's existing theme button cycles
 *  through dark → light → oled, but a settings-page picker is more
 *  discoverable and lets the user jump straight to OLED without
 *  cycling through light. */
function ThemePicker() {
  const tr = useTr();
  const theme = useThemeStore((s) => s.theme);
  const setTheme = useThemeStore((s) => s.setTheme);
  const options: Array<{
    value: Theme;
    label: string;
    description: string;
    icon: React.ReactNode;
  }> = [
    {
      value: "dark",
      label: tr("theme_dark", undefined, "Dark"),
      description: tr(
        "theme_dark_hint",
        undefined,
        "Default. Slight surface elevation, balanced contrast.",
      ),
      icon: <Moon size={14} />,
    },
    {
      value: "light",
      label: tr("theme_light", undefined, "Light"),
      description: tr(
        "theme_light_hint",
        undefined,
        "Daytime colours, high readability.",
      ),
      icon: <Sun size={14} />,
    },
    {
      value: "oled",
      label: tr("theme_oled", undefined, "OLED"),
      description: tr(
        "theme_oled_hint",
        undefined,
        "Pure-#000 background. Mitigates burn-in on OLED panels.",
      ),
      icon: <MoonStar size={14} />,
    },
  ];
  return (
    <div className="space-y-2">
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          onClick={() => setTheme(o.value)}
          className={`flex w-full items-start gap-2 rounded-md border p-2 text-left ${
            theme === o.value
              ? "border-[var(--color-accent)] bg-[var(--color-surface)]"
              : "border-[var(--color-border)] hover:bg-[var(--color-surface)]"
          }`}
        >
          <span className="mt-0.5">{o.icon}</span>
          <span className="min-w-0 flex-1">
            <div className="text-sm font-medium">{o.label}</div>
            <div className="text-[11px] text-[var(--color-muted)]">
              {o.description}
            </div>
          </span>
          {theme === o.value && (
            <CheckCircle2 size={12} className="text-[var(--color-good)]" />
          )}
        </button>
      ))}
    </div>
  );
}

/** Bandwidth-throttle hint. The actual per-job throttle UI lives in
 *  the Upload screen (BandwidthCard) and is wired end-to-end through
 *  the renderer → Tauri command → engine HTTP body since the most
 *  recent fix. The env-var fallback (FTX2_BANDWIDTH_MBPS) still works
 *  for headless engine runs but is no longer the user-facing path. */
function BandwidthHelp() {
  const tr = useTr();
  return (
    <div className="space-y-2 text-sm">
      <div className="flex items-start gap-2">
        <Gauge size={14} className="mt-0.5 shrink-0 text-[var(--color-muted)]" />
        <div className="min-w-0 flex-1">
          <div className="font-medium">
            {tr("settings_bandwidth_title", undefined, "Bandwidth throttle")}
          </div>
          <div className="mt-0.5 text-xs text-[var(--color-muted)]">
            {tr(
              "settings_bandwidth_hint",
              undefined,
              "Cap outbound transfer speed to leave headroom for video calls / game streaming. Set the cap on the Upload screen — it applies to the next started transfer. The FTX2_BANDWIDTH_MBPS env var still works for headless engine runs.",
            )}
          </div>
          <div className="mt-2 rounded-md bg-[var(--color-surface)] px-2 py-1 font-mono text-[11px]">
            FTX2_BANDWIDTH_MBPS=10  # headless / scripted only
          </div>
        </div>
      </div>
    </div>
  );
}

/**
 * One-click bug report bundle. Pulls together everything a maintainer
 * usually asks for in an issue: app version, OS info, recent log
 * entries, payload status, last 50 activity entries, last 20
 * notifications, redacted user config (no IPs / hostnames). Writes
 * to a user-picked .json path.
 *
 * Lives entirely in the renderer — every store this needs is already
 * in the renderer state. No new payload calls.
 */
function BugReportButton() {
  const tr = useTr();
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  /** Per-source counts shown next to the Save button so users can
   *  see what's about to leave their machine. Computed lazily on the
   *  first build click and refreshed on each subsequent click. */
  const [summary, setSummary] = useState<{
    logs: number;
    activity: number;
    notifications: number;
    rosterPS5s: number;
    schedules: number;
    playTimeTitles: number;
  } | null>(null);

  async function build() {
    setBusy(true);
    setError(null);
    setResult(null);
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const { writeTextFileToPath } = await import("../../lib/saveTextFile");
      const { getVersion } = await import("@tauri-apps/api/app");
      const { buildDiagnosticBundle } = await import(
        "../../lib/diagnosticBundle"
      );

      const appVersion = await getVersion().catch(() => "unknown");
      const bundle = buildDiagnosticBundle({ appVersion, redact: true });
      // Surface a per-source summary so the user knows what's in
      // the bundle before sharing. Counts come straight from the
      // bundle so the displayed numbers always match the file.
      setSummary({
        logs: bundle.recent_logs.length,
        activity: bundle.recent_activity.length,
        notifications: bundle.recent_notifications.length,
        rosterPS5s: bundle.roster.length,
        schedules: bundle.schedules_count,
        playTimeTitles: bundle.play_time_titles,
      });

      const dest = await save({
        defaultPath: `ps5upload-bug-report-${Date.now()}.json`,
        filters: [{ name: "JSON", extensions: ["json"] }],
      });
      if (!dest || typeof dest !== "string") {
        setBusy(false);
        return;
      }
      await writeTextFileToPath(dest, JSON.stringify(bundle, null, 2));
      setResult(dest);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="text-sm">
      <div className="font-medium">
        {tr("bug_report_title", undefined, "Save bug report bundle")}
      </div>
      <div className="mt-0.5 text-xs text-[var(--color-muted)]">
        {tr(
          "bug_report_hint",
          undefined,
          "One-click JSON bundle with app version, OS, recent logs, activity history, and connection state. PS5 IP is stripped — safe to attach to a public issue.",
        )}
      </div>
      <button
        type="button"
        onClick={build}
        disabled={busy}
        className="mt-3 inline-flex items-center gap-1.5 rounded-md bg-[var(--color-accent)] px-3 py-1.5 text-xs text-[var(--color-accent-contrast)] disabled:opacity-50"
      >
        {busy ? (
          <Loader2 size={11} className="animate-spin" />
        ) : (
          <Download size={11} />
        )}
        {busy
          ? tr("bug_report_busy", undefined, "Building…")
          : tr("bug_report_action", undefined, "Save bug report")}
      </button>
      {result && (
        <div className="mt-2 flex items-center gap-1 text-[11px] text-[var(--color-good)]">
          <CheckCircle2 size={11} />
          {tr("bug_report_done", { path: result }, `Saved to ${result}`)}
        </div>
      )}
      {error && (
        <div className="mt-2 flex items-start gap-1 text-[11px] text-[var(--color-bad)]">
          <AlertTriangle size={11} className="mt-0.5 shrink-0" />
          {error}
        </div>
      )}
      {summary && (
        <div className="mt-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2 text-[11px]">
          <div className="mb-1 font-medium text-[var(--color-muted)]">
            {tr("bug_report_includes", undefined, "Bundle includes")}
          </div>
          <ul className="grid grid-cols-2 gap-x-4 gap-y-0.5 tabular-nums text-[10px]">
            <li>
              {summary.logs} {tr("bug_report_logs", undefined, "log entries")}
            </li>
            <li>
              {summary.activity}{" "}
              {tr("bug_report_activity", undefined, "activity rows")}
            </li>
            <li>
              {summary.notifications}{" "}
              {tr("bug_report_notifs", undefined, "notifications")}
            </li>
            <li>
              {summary.rosterPS5s}{" "}
              {tr("bug_report_ps5s", undefined, "PS5 profiles (host redacted)")}
            </li>
            <li>
              {summary.schedules}{" "}
              {tr("bug_report_schedules", undefined, "schedules")}
            </li>
            <li>
              {summary.playTimeTitles}{" "}
              {tr("bug_report_playtime", undefined, "playtime entries")}
            </li>
          </ul>
        </div>
      )}
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
            {tr("update_downloaded_title", undefined, "Downloaded. Finish installing:")}
          </div>
          <div className="mb-2 font-mono text-[var(--color-text)]">
            {phase.download.path}
          </div>
          <ol className="list-inside list-decimal space-y-0.5 text-[var(--color-muted)]">
            <li>{tr("update_step_quit", undefined, "Quit PS5Upload.")}</li>
            <li>
              {pickInstallHint(phase.download.path, tr)}
            </li>
            <li>{tr("update_step_launch", undefined, "Launch the new version.")}</li>
          </ol>
          <button
            type="button"
            onClick={dismissDownload}
            className="mt-2 text-[10px] underline-offset-2 hover:underline"
          >
            {tr("update_dismiss", undefined, "Dismiss")}
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
              "Download v{version}",
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
 *  `navigator.userAgent` at runtime. Takes a `tr` so it can pull the
 *  current-language string at call time (the function isn't a React
 *  component and can't use the hook directly). */
function pickInstallHint(
  downloadPath: string,
  tr: (key: string, vars?: Record<string, string | number>, fallback?: string) => string,
): string {
  const lower = downloadPath.toLowerCase();
  if (lower.endsWith(".dmg")) {
    return tr(
      "install_hint_dmg",
      undefined,
      "Double-click the .dmg, drag PS5Upload into Applications, replace when asked.",
    );
  }
  if (lower.endsWith(".zip")) {
    return tr(
      "install_hint_zip",
      undefined,
      "Extract the zip and replace the existing PS5Upload with the new one.",
    );
  }
  return tr(
    "install_hint_generic",
    undefined,
    "Replace the existing PS5Upload with the newly downloaded file.",
  );
}

/** One-line summary of the current phase — the line the sidebar badge
 *  corresponds to. Kept minimal so the eye lands on the CTA below it. */
function StatusRow({ phase }: { phase: UpdatePhase }) {
  const tr = useTr();
  if (phase.kind === "idle") {
    return (
      <div className="text-xs text-[var(--color-muted)]">
        {tr(
          "update_idle",
          undefined,
          "Haven't checked yet — the app checks once per launch in the background.",
        )}
      </div>
    );
  }
  if (phase.kind === "checking") {
    return (
      <div className="inline-flex items-center gap-2 text-xs text-[var(--color-muted)]">
        <Loader2 size={12} className="animate-spin" />
        {tr("update_checking", undefined, "Checking GitHub releases…")}
      </div>
    );
  }
  if (phase.kind === "error") {
    return (
      <div className="inline-flex items-start gap-2 text-xs text-[var(--color-bad)]">
        <AlertTriangle size={12} className="mt-0.5 shrink-0" />
        <span>{tr("update_check_error", { message: phase.message }, `Couldn't check for updates: ${phase.message}`)}</span>
      </div>
    );
  }
  if (phase.kind === "up-to-date") {
    return (
      <div className="inline-flex items-center gap-2 text-xs text-[var(--color-good)]">
        <CheckCircle2 size={12} />
        {tr(
          "update_up_to_date",
          { version: phase.result.current_version },
          `You're on the latest (v${phase.result.current_version}).`,
        )}
      </div>
    );
  }
  if (phase.kind === "available") {
    return (
      <div className="inline-flex items-center gap-2 text-xs text-[var(--color-accent)]">
        <Download size={12} />
        {tr(
          "update_available_msg",
          { latest: phase.result.latest_version, current: phase.result.current_version },
          `v${phase.result.latest_version} is available — you're on v${phase.result.current_version}.`,
        )}
      </div>
    );
  }
  if (phase.kind === "downloading") {
    return (
      <div className="inline-flex items-center gap-2 text-xs text-[var(--color-accent)]">
        <Loader2 size={12} className="animate-spin" />
        {tr(
          "update_downloading",
          { version: phase.result.latest_version },
          `Downloading v${phase.result.latest_version}…`,
        )}
      </div>
    );
  }
  if (phase.kind === "downloaded") {
    return (
      <div className="inline-flex items-center gap-2 text-xs text-[var(--color-good)]">
        <CheckCircle2 size={12} />
        {tr(
          "update_downloaded_status",
          { version: phase.result.latest_version },
          `v${phase.result.latest_version} downloaded — installer opened.`,
        )}
      </div>
    );
  }
  // download-failed
  return (
    <div className="inline-flex items-start gap-2 text-xs text-[var(--color-bad)]">
      <AlertTriangle size={12} className="mt-0.5 shrink-0" />
      <span>{tr("update_download_error", { message: phase.message }, `Download failed: ${phase.message}`)}</span>
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
  const tr = useTr();
  if (!notes.trim()) return null;
  return (
    <div className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-3">
      <div className="mb-1 flex items-center justify-between text-[10px] uppercase tracking-wide text-[var(--color-muted)]">
        <span>{tr("whats_new_in", { version }, `What's new in v${version}`)}</span>
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
