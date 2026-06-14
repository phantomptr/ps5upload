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
  Flower2,
  Gauge,
  Bell,
  Bug,
  Zap,
  Trash2,
  ExternalLink,
  ALargeSmall,
} from "lucide-react";
import { Link } from "react-router-dom";
import { useThemeStore, type Theme } from "../../state/theme";
import {
  useUiScaleStore,
  UI_SCALE_STEPS,
  uiScaleLabel,
} from "../../state/uiScale";

import { PageHeader } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";

import { useKeepAwakeStore } from "../../state/keepAwake";
import { useNotificationsStore } from "../../state/notifications";
import { useLangStore, useTr, LANGUAGES } from "../../state/lang";
import {
  useUploadSettingsStore,
  MAX_UPLOAD_STREAMS,
} from "../../state/uploadSettings";
import { useConnectionStore } from "../../state/connection";
import { useEngineStore, DEFAULT_ENGINE_URL } from "../../state/engine";
import { userConfigPath, resetAllAppData } from "../../state/userConfig";
import { useUpdateStore, type UpdatePhase } from "../../state/update";
import { isMobile } from "../../lib/platform";
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

/** Full-width band heading that visually groups the cards that follow it.
 *  Implemented as a `col-span-full` sibling in the same grid (not a wrapper)
 *  so it forces a new row and the cards under it flow as their own band. */
function GroupHeading({ children }: { children: React.ReactNode }) {
  return (
    <h2 className="mt-2 border-b border-[var(--color-border)] pb-1.5 text-sm font-semibold text-[var(--color-text)] md:col-span-2 xl:col-span-3">
      {children}
    </h2>
  );
}

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

/** Engine base URL field. Defaults to the bundled local sidecar; a power
 *  user can point it at a remote/self-hosted engine (e.g. the Docker
 *  image). Edits commit on blur so trailing-slash normalisation doesn't
 *  fight typing. */
function EngineUrlSection() {
  const tr = useTr();
  const engineUrl = useEngineStore((s) => s.engineUrl);
  const setEngineUrl = useEngineStore((s) => s.setEngineUrl);
  const [draft, setDraft] = useState(engineUrl);

  return (
    <Section title={tr("settings_card_engine", undefined, "Engine")}>
      <label className="grid gap-1.5 text-sm">
        <span className="font-medium">
          {tr("engine_url_label", undefined, "Engine URL")}
        </span>
        <div className="flex items-center gap-2">
          <input
            type="text"
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onBlur={() => setEngineUrl(draft)}
            placeholder={DEFAULT_ENGINE_URL}
            spellCheck={false}
            className="flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-1.5 text-sm"
          />
          {engineUrl !== DEFAULT_ENGINE_URL && (
            <button
              type="button"
              onClick={() => {
                setDraft(DEFAULT_ENGINE_URL);
                setEngineUrl(DEFAULT_ENGINE_URL);
              }}
              className="text-xs text-[var(--color-muted)] hover:text-[var(--color-text)]"
            >
              {tr("engine_url_reset", undefined, "Reset")}
            </button>
          )}
        </div>
        <span className="text-xs text-[var(--color-muted)]">
          {tr(
            "engine_url_hint",
            undefined,
            "Where the app reaches the ps5upload-engine. Leave as the default local sidecar, or point at a remote/self-hosted engine. Restart the app after switching between local and remote.",
          )}
        </span>
      </label>
    </Section>
  );
}

export default function SettingsScreen() {
  const { enabled, supported, lastError, setEnabled, syncFromBackend } =
    useKeepAwakeStore();
  const osNotifyEnabled = useNotificationsStore((s) => s.osNotifyEnabled);
  const setOsNotifyEnabled = useNotificationsStore((s) => s.setOsNotifyEnabled);
  const lang = useLangStore((s) => s.lang);
  const setLang = useLangStore((s) => s.setLang);
  const tr = useTr();
  const {
    alwaysOverwrite,
    showTransferFiles,
    uploadStreams,
    autoResume,
    keepPs5AwakeMode,
    bandwidthCapMbps,
    setAlwaysOverwrite,
    setShowTransferFiles,
    setUploadStreams,
    setAutoResume,
    setKeepPs5AwakeMode,
    setBandwidthCapMbps,
  } = useUploadSettingsStore();
  const payloadMaxStreams = useConnectionStore((s) => s.maxTransferStreams);
  // UA-based, stable for the whole session — safe to read during render.
  const mobile = isMobile();

  const [cfgPath, setCfgPath] = useState<string | null>(null);

  useEffect(() => {
    syncFromBackend();
    // Resolve the settings-mirror path so the Settings → Storage card
    // can show the user where their settings.json lives. One-shot per
    // mount — the path is stable once the OS tells us the home dir.
    userConfigPath()
      .then(setCfgPath)
      .catch(() => setCfgPath(null));
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
      <div className="grid items-start gap-4 md:grid-cols-2 xl:grid-cols-3">
        <GroupHeading>
          {tr("settings_group_general", undefined, "General")}
        </GroupHeading>

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

        <Section
          title={tr("settings_section_appearance", undefined, "Appearance")}
        >
          <ThemePicker />
        </Section>

        <Section
          title={tr("settings_section_text_size", undefined, "Text size")}
        >
          <TextSizePicker />
        </Section>

        {/* Mobile wording: the keep-awake store drives a screen wake
            lock on Android (state/keepAwake.ts), so the desktop
            "keep computer awake" copy is wrong there. `supported` is
            already accurate on Android — it reflects the WebView's
            wake-lock API, not the desktop inhibitor — so the
            "not supported" warning only shows when the wake lock is
            genuinely unavailable. */}
        <Section
          title={
            mobile
              ? tr("keep_awake_mobile", undefined, "Keep screen on")
              : tr("keep_awake", undefined, "Keep computer awake")
          }
        >
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
                {mobile
                  ? tr(
                      "keep_awake_on_mobile",
                      undefined,
                      "Keep the screen on while PS5 Upload is in the foreground",
                    )
                  : tr(
                      "keep_awake_on",
                      undefined,
                      "Keep the computer awake while PS5 Upload is open",
                    )}
              </div>
              <div className="mt-0.5 text-xs text-[var(--color-muted)]">
                {mobile
                  ? tr(
                      "keep_awake_hint_mobile",
                      undefined,
                      "Stops the display from dimming and locking while the app is visible — useful during long uploads. Released automatically when you switch apps or turn the screen off.",
                    )
                  : tr(
                      "keep_awake_hint",
                      undefined,
                      "Uploads, downloads, and installs already keep the computer awake automatically. Turn this on to also block sleep while the app is open but idle.",
                    )}
              </div>
              {!supported && (
                <div className="mt-1 text-xs text-[var(--color-warn)]">
                  {mobile
                    ? tr(
                        "keep_awake_unsupported_mobile",
                        undefined,
                        "Screen wake lock isn't available in this WebView.",
                      )
                    : tr(
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

        <EngineUrlSection />

        <GroupHeading>
          {tr("settings_group_uploads", undefined, "Uploads")}
        </GroupHeading>

        <Section
          title={tr("settings_card_upload_behavior", undefined, "Behavior")}
        >
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
                  {tr(
                    "always_overwrite",
                    undefined,
                    "Always overwrite without asking",
                  )}
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
                  {tr(
                    "show_file_list",
                    undefined,
                    "Show file list during transfer",
                  )}
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

            <label className="flex cursor-pointer items-start gap-3 text-sm">
              <input
                type="checkbox"
                className="mt-0.5 h-4 w-4"
                checked={autoResume}
                onChange={(e) => setAutoResume(e.target.checked)}
              />
              <div>
                <div className="font-medium">
                  {tr(
                    "upload_auto_resume",
                    undefined,
                    "Auto-resume uploads after a failure",
                  )}
                </div>
                <div className="mt-0.5 text-xs text-[var(--color-muted)]">
                  {tr(
                    "upload_auto_resume_hint",
                    undefined,
                    "If an upload drops mid-transfer — most often because the PS5 payload crashed — automatically re-send the payload and resume from where it left off, retrying a few times before giving up. Fatal problems like the PS5 running out of space still stop right away. On by default.",
                  )}
                </div>
              </div>
            </label>

            <div className="text-sm">
              <div className="flex items-center gap-2 font-medium">
                <Zap size={14} />
                <label htmlFor="keep-ps5-awake-mode">
                  {tr("keep_ps5_awake_mode", "Keep the PS5 awake")}
                </label>
                <select
                  id="keep-ps5-awake-mode"
                  value={keepPs5AwakeMode}
                  onChange={(e) =>
                    setKeepPs5AwakeMode(
                      e.target.value as "off" | "transfers" | "always",
                    )
                  }
                  className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 text-xs"
                >
                  <option value="off">
                    {tr("keep_awake_mode_off", "Off")}
                  </option>
                  <option value="transfers">
                    {tr("keep_awake_mode_transfers", "During transfers")}
                  </option>
                  <option value="always">
                    {tr("keep_awake_mode_always", "Always while connected")}
                  </option>
                </select>
              </div>
              <div className="mt-0.5 text-xs text-[var(--color-muted)]">
                {tr(
                  "keep_ps5_awake_mode_hint",
                  "Periodically resets the PS5's auto-standby timer so it can't drop into rest mode. “During transfers” protects long uploads (a common cause of failed uploads; default). “Always while connected” keeps every console with a running helper out of rest mode for as long as the app is open. Putting the PS5 to rest manually still works in every mode.",
                )}
              </div>
            </div>
          </div>
        </Section>

        <Section title={tr("settings_card_upload_speed", undefined, "Speed")}>
          <div className="grid gap-3">
            <div className="text-sm">
              <div className="flex items-center gap-3">
                <label htmlFor="upload-streams" className="font-medium">
                  {tr("upload_streams", undefined, "Parallel upload streams")}
                </label>
                <select
                  id="upload-streams"
                  value={uploadStreams}
                  onChange={(e) => setUploadStreams(Number(e.target.value))}
                  className="rounded border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 text-sm"
                >
                  {Array.from(
                    { length: MAX_UPLOAD_STREAMS },
                    (_, i) => i + 1,
                  ).map((n) => (
                    <option key={n} value={n}>
                      {n === 1
                        ? tr("upload_streams_off", undefined, "1 (single)")
                        : `${n}`}
                    </option>
                  ))}
                </select>
              </div>
              <div className="mt-0.5 text-xs text-[var(--color-muted)]">
                {tr(
                  "upload_streams_hint",
                  undefined,
                  "Upload large folders over several connections at once. A single stream caps around 40 MB/s on non-Pro consoles (a single-thread limit, not your network or SSD); more streams aggregate toward your wired-LAN ceiling. Only the connected payload's supported maximum is used.",
                )}
              </div>
              {uploadStreams > 1 &&
                (payloadMaxStreams == null || payloadMaxStreams < 2) && (
                  <div className="mt-1 text-xs text-[var(--color-warn,#b8860b)]">
                    {tr(
                      "upload_streams_unsupported",
                      undefined,
                      "The connected payload doesn't support multiple streams yet — uploads will use a single stream until it's updated.",
                    )}
                  </div>
                )}
              {uploadStreams > 1 &&
                payloadMaxStreams != null &&
                payloadMaxStreams >= 2 && (
                  <div className="mt-1 text-xs text-[var(--color-warn,#b8860b)]">
                    {tr(
                      "upload_streams_unstable",
                      undefined,
                      "Faster, but use with caution: more than 1 stream runs several transfers against the PS5 at once, and on some consoles that can crash the payload mid-upload (it then stops responding until you reload it). If uploads start failing or the payload drops, set this back to 1 — the single-stream path is the most stable.",
                    )}
                  </div>
                )}
            </div>

            <BandwidthControl
              value={bandwidthCapMbps}
              onChange={setBandwidthCapMbps}
            />
          </div>
        </Section>

        <GroupHeading>
          {tr("settings_section_notifications", undefined, "Notifications")}
        </GroupHeading>

        <Section title={tr("notifications", undefined, "Notifications")}>
          <label className="flex items-start gap-3 text-sm">
            <input
              type="checkbox"
              checked={osNotifyEnabled}
              onChange={(e) => setOsNotifyEnabled(e.target.checked)}
              className="mt-0.5 accent-[var(--color-accent)]"
            />
            <div>
              <div className="flex items-center gap-2 font-medium">
                <Bell size={14} />
                {tr("os_notify_label", undefined, "Show system notifications")}
              </div>
              <div className="mt-0.5 text-xs text-[var(--color-muted)]">
                {tr(
                  "os_notify_hint",
                  undefined,
                  "Mirror in-app notifications (transfer done, errors, etc.) to your operating system's notification center — but only when the app is in the background, so you're not notified twice. You may be asked to grant notification permission.",
                )}
              </div>
            </div>
          </label>
          <div className="my-3 border-t border-[var(--color-border)]" />
          <NotifPrunePanel />
        </Section>

        <GroupHeading>
          {tr("settings_group_updates", undefined, "Updates")}
        </GroupHeading>

        {/* Updates takes the full row — the release-notes blob can
            be multiple paragraphs and deserves reading space. The
            "updates" i18n key already exists across every locale in
            i18n.ts; inner strings fall back to English for now. */}
        <Section title={tr("updates", undefined, "Updates")} full>
          <UpdatesPanel />
        </Section>

        <GroupHeading>
          {tr("settings_group_data", undefined, "Data & reset")}
        </GroupHeading>

        {/* Storage is the long row — path text is wide, so it takes
            the whole grid width. */}
        <Section title={tr("storage", undefined, "Storage")} full>
          <div className="flex items-start gap-3 text-sm">
            <FileJson
              size={14}
              className="mt-0.5 shrink-0 text-[var(--color-muted)]"
            />
            <div className="min-w-0 flex-1">
              <div className="font-medium">
                {tr("settings_file", undefined, "Settings file")}
              </div>
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

        <Section
          title={tr("settings_section_backup", undefined, "Backup / restore")}
          full
        >
          <BackupRestorePanel />
        </Section>

        <Section title={tr("settings_section_reset", undefined, "Reset")} full>
          <ResetPanel />
        </Section>

        <GroupHeading>
          {tr("settings_section_diagnostic", undefined, "Diagnostics")}
        </GroupHeading>

        {/* The band heading already says "Diagnostics"; the single card
            inside it gets its own specific title so the screen doesn't
            stack the same word twice. */}
        <Section title={tr("bug_report", undefined, "Bug report")} full>
          <BugReportLink />
        </Section>

        <GroupHeading>
          {tr("settings_group_automation", undefined, "Automation")}
        </GroupHeading>

        {/* 2.12.0: Audit log promoted out of Settings to its own
            top-level route (/audit-log) under Diagnostics. AuditLogPanel
            removed too. */}
        <Section
          title={tr("settings_section_schedules", undefined, "Schedules")}
          full
        >
          <SchedulesPanel />
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
  const [actionDraft, setActionDraft] = useState<"power_tick" | "notif">(
    "notif",
  );

  function add() {
    if (!labelDraft.trim() || !/^\d\d:\d\d$/.test(hhmmDraft)) return;
    useScheduleStore.getState().add({
      enabled: true,
      kind: "daily",
      action: actionDraft,
      hhmm: hhmmDraft,
      label: labelDraft.trim(),
      // power_tick targets a specific console — capture the active one now
      // so the tick fires against the PS5 this schedule was made for, not
      // whatever tab happens to be active when it triggers.
      host:
        actionDraft === "power_tick"
          ? useConnectionStore.getState().host?.trim() || undefined
          : undefined,
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
      <p className="text-xs text-[var(--color-muted)]">
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
                {s.kind === "daily" &&
                  tr(
                    "schedule_daily_at",
                    { time: s.hhmm ?? "" },
                    `daily at ${s.hhmm ?? ""}`,
                  )}
                {s.kind === "weekly" &&
                  tr(
                    "schedule_weekly_at",
                    { time: s.hhmm ?? "" },
                    `weekly at ${s.hhmm ?? ""}`,
                  )}
                {s.kind === "once" &&
                  s.oneShotMs &&
                  tr(
                    "schedule_once_at",
                    { time: new Date(s.oneShotMs).toLocaleString() },
                    `once at ${new Date(s.oneShotMs).toLocaleString()}`,
                  )}
                {" · "}
                {/* Friendly action name — reuse the same labels as the
                    add-dropdown below, instead of the raw store id
                    (notif / power_tick) that meant nothing to the user. */}
                {s.action === "power_tick"
                  ? tr("settings_ps5_power_tick", "PS5 power tick")
                  : tr("settings_notify_only", "Notify only")}
                {s.action === "power_tick" && s.host && ` → ${s.host}`}
              </span>
              <button
                type="button"
                onClick={() => remove(s.id)}
                className="ml-auto text-xs text-[var(--color-muted)] hover:text-[var(--color-bad)]"
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
          onChange={(e) =>
            setActionDraft(e.target.value as "notif" | "power_tick")
          }
          className="rounded border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 text-xs"
        >
          <option value="notif">
            {tr("settings_notify_only", "Notify only")}
          </option>
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
      <div className="text-xs text-[var(--color-muted)]">
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

  // Backup captures EVERY persisted key in the app's `ps5upload.` namespace via
  // a prefix scan, rather than a hand-maintained allowlist. The old allowlist
  // had drifted: it referenced "ps5upload.play_time.v1" while the store had
  // moved to ".v2" (playtime silently lost on migration), omitted actively
  // persisted keys (log_level, osNotify, install prefs, keep-awake mode,
  // register-after-upload, …), and listed phantom keys no store writes. A
  // prefix scan is immune to that whole class of drift — any new store backed by
  // a `ps5upload.`-prefixed key is captured automatically.
  const NS_PREFIX = "ps5upload.";

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
      for (let i = 0; i < window.localStorage.length; i++) {
        const k = window.localStorage.key(i);
        if (k && k.startsWith(NS_PREFIX)) {
          ls[k] = window.localStorage.getItem(k);
        }
      }
      const fileName = `ps5upload-settings-${Date.now()}.json`;
      const dest = await save({
        defaultPath: fileName,
        filters: [{ name: "JSON", extensions: ["json"] }],
      });
      if (!dest || typeof dest !== "string") return;
      await writeTextFileToPath(
        dest,
        JSON.stringify(bundle, null, 2),
        fileName,
      );
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
      for (const k of Object.keys(ls)) {
        // Safety: only ever touch our own namespace, so a hand-edited or
        // malicious bundle can't write arbitrary localStorage keys.
        if (!k.startsWith(NS_PREFIX)) continue;
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
        <div className="flex items-center gap-1 text-xs text-[var(--color-good)]">
          <CheckCircle2 size={11} />
          {info}
        </div>
      )}
      {error && (
        <div className="flex items-start gap-1 text-xs text-[var(--color-bad)]">
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
      label: tr("theme_dark", undefined, "PS5 Dark"),
      description: tr(
        "theme_dark_hint",
        undefined,
        "Default. The console's black-plastic charcoal, PlayStation-blue accent.",
      ),
      icon: <Moon size={14} />,
    },
    {
      value: "light",
      label: tr("theme_light", undefined, "PS5 Light"),
      description: tr(
        "theme_light_hint",
        undefined,
        "The console's white-plastic panels — clean, bright, cool white.",
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
    {
      value: "rose",
      label: tr("theme_rose", undefined, "Rose"),
      description: tr(
        "theme_rose_hint",
        undefined,
        "Warm, bright, and soft — built around a bold rose accent.",
      ),
      icon: <Flower2 size={14} />,
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
            <div className="text-xs text-[var(--color-muted)]">
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

/** Lets the user rescale the whole UI. Every size in the app is rem-based off
 *  one root font-size, so this one control resizes text, padding, icons, and
 *  controls together — the durable, cross-platform fix for Android's system
 *  Font/Display-size inflation clipping list rows to "ps5-image…". */
function TextSizePicker() {
  const tr = useTr();
  const scale = useUiScaleStore((s) => s.scale);
  const setScale = useUiScaleStore((s) => s.setScale);
  return (
    <div className="space-y-2">
      <div className="flex items-center gap-2 text-xs text-[var(--color-muted)]">
        <ALargeSmall size={14} />
        {tr(
          "settings_text_size_hint",
          undefined,
          "Resize the whole app. Pick a smaller size if text is cut off (“ps5-image…”) on your phone.",
        )}
      </div>
      {/* Stepper row: each button is a percentage of the designed size. The
          buttons themselves sit OUTSIDE the rem flow (fixed text-[13px]) so
          they stay tappable even when the user has scaled the app very small. */}
      <div className="flex flex-wrap gap-1.5">
        {UI_SCALE_STEPS.map((step) => {
          const active = Math.abs(step - scale) < 0.001;
          return (
            <button
              key={step}
              type="button"
              onClick={() => setScale(step)}
              aria-pressed={active}
              className={`min-w-[3rem] rounded-md border px-2 py-1.5 text-center text-[13px] ${
                active
                  ? "border-[var(--color-accent)] bg-[var(--color-surface)] font-semibold"
                  : "border-[var(--color-border)] hover:bg-[var(--color-surface)]"
              }`}
            >
              {uiScaleLabel(step)}
              {Math.abs(step - 1) < 0.001 && (
                <span className="ml-1 text-[10px] text-[var(--color-muted)]">
                  {tr("settings_text_size_default", undefined, "default")}
                </span>
              )}
            </button>
          );
        })}
      </div>
    </div>
  );
}

/** Upload speed limit (bandwidth cap). A real control now — the old version
 *  was just a blurb pointing at the Upload screen + an env var. 0 = no limit.
 *  Shares the same `bandwidthCapMbps` store the Upload screen reads, so a cap
 *  set here applies to the next started transfer. */
function BandwidthControl({
  value,
  onChange,
}: {
  value: number;
  onChange: (n: number) => void;
}) {
  const tr = useTr();
  return (
    <div className="text-sm">
      <div className="flex items-center gap-2">
        <Gauge size={14} className="shrink-0 text-[var(--color-muted)]" />
        <label htmlFor="bw-cap" className="font-medium">
          {tr("bandwidth_cap_label", undefined, "Upload speed limit")}
        </label>
        <input
          id="bw-cap"
          type="number"
          min={0}
          step={1}
          value={value || ""}
          placeholder="0"
          onChange={(e) =>
            onChange(Math.max(0, Math.floor(Number(e.target.value) || 0)))
          }
          className="w-20 rounded border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 text-sm tabular-nums"
        />
        <span className="text-xs text-[var(--color-muted)]">MB/s</span>
      </div>
      <div className="mt-0.5 text-xs text-[var(--color-muted)]">
        {value > 0
          ? tr(
              "bandwidth_cap_on_hint",
              { n: value },
              `Uploads are capped at ${value} MB/s. Set to 0 to remove the limit.`,
            )
          : tr(
              "bandwidth_cap_off_hint",
              undefined,
              "No limit. Set a cap if uploads saturate your Wi-Fi/LAN and make other devices laggy — it throttles the upload to leave headroom. Applies to the next started transfer.",
            )}
      </div>
    </div>
  );
}

/**
 * Danger zone: factory-reset. Deletes ALL local data + metadata (settings,
 * roster, history, queues, caches, crash reports) and reloads the app fresh.
 * Nothing on the PS5 is touched. Gated behind a destructive confirm dialog.
 */
function ResetPanel() {
  const tr = useTr();
  const { confirm, dialog } = useConfirm();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function doReset() {
    const ok = await confirm({
      title: tr("reset_confirm_title", undefined, "Reset PS5Upload?"),
      message: tr(
        "reset_confirm_body",
        undefined,
        "This permanently deletes ALL local PS5Upload data — settings, your saved PS5s, activity/queue history, payload caches, and crash reports. It does NOT touch anything on your PS5. The app reloads to a fresh state. This can't be undone.",
      ),
      confirmLabel: tr("reset_confirm_action", undefined, "Delete everything"),
      destructive: true,
    });
    if (!ok) return;
    setBusy(true);
    setError(null);
    try {
      await resetAllAppData();
      // Reload so every store re-initialises from defaults.
      window.location.reload();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  }

  return (
    <div className="space-y-3 text-sm">
      <p className="text-xs text-[var(--color-muted)]">
        {tr(
          "reset_hint",
          undefined,
          "Wipe every local trace of PS5Upload and start over: settings, saved PS5s, activity/queue history, payload caches, and crash reports. Nothing on your PS5 is affected. Export a backup above first if you might want it back.",
        )}
      </p>
      <button
        type="button"
        onClick={doReset}
        disabled={busy}
        className="inline-flex items-center gap-1.5 rounded-md border border-[var(--color-bad)] px-3 py-1.5 text-xs text-[var(--color-bad)] hover:bg-[var(--color-bad)] hover:text-[var(--color-accent-contrast)] disabled:opacity-50"
      >
        {busy ? (
          <Loader2 size={11} className="animate-spin" />
        ) : (
          <Trash2 size={11} />
        )}
        {tr("reset_action", undefined, "Reset all settings & data")}
      </button>
      {error && (
        <div className="flex items-start gap-1 text-xs text-[var(--color-bad)]">
          <AlertTriangle size={11} className="mt-0.5 shrink-0" />
          {error}
        </div>
      )}
      {dialog}
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
/**
 * Diagnostics moved to a dedicated Bug Report page (sidebar → Diagnostics →
 * Bug report). It owns the richer flow — description, screenshots, log time
 * window + level, PS5 snapshot, and the one-click `.zip`. Settings just points
 * there so there's a single place to file a report.
 */
function BugReportLink() {
  const tr = useTr();
  return (
    <div className="text-sm">
      <div className="font-medium">
        {tr("settings_bug_report_title", undefined, "Report a bug")}
      </div>
      <div className="mt-0.5 text-xs text-[var(--color-muted)]">
        {tr(
          "settings_bug_report_hint",
          undefined,
          "Describe the issue, attach screenshots, and package logs + a PS5 snapshot into one .zip to post on Discord.",
        )}
      </div>
      <Link
        to="/bug-report"
        className="mt-3 inline-flex items-center gap-1.5 rounded-md bg-[var(--color-accent)] px-3 py-1.5 text-xs text-[var(--color-accent-contrast)]"
      >
        <Bug size={11} />
        {tr("settings_bug_report_open", undefined, "Open Bug Report")}
      </Link>
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
  const autoCheckEnabled = useUpdateStore((s) => s.autoCheckEnabled);
  const setAutoCheckEnabled = useUpdateStore((s) => s.setAutoCheckEnabled);
  // Mobile: the store's download() opens the APK URL (or the release
  // page) in the system browser instead of saving to ~/Downloads —
  // that flow can't work in Android's sandbox. Relabel the CTA so it
  // doesn't promise an in-app download, and show it even when the
  // manifest has no APK asset (the release page is always a target).
  const mobile = isMobile();

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
    resultForNotes?.download_url && resultForNotes.download_url.length > 0;

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
            {tr(
              "update_downloaded_title",
              undefined,
              "Downloaded. Finish installing:",
            )}
          </div>
          <div className="mb-2 font-mono text-[var(--color-text)]">
            {phase.download.path}
          </div>
          <ol className="list-inside list-decimal space-y-0.5 text-[var(--color-muted)]">
            <li>{tr("update_step_quit", undefined, "Quit PS5Upload.")}</li>
            <li>{pickInstallHint(phase.download.path, tr)}</li>
            <li>
              {tr("update_step_launch", undefined, "Launch the new version.")}
            </li>
          </ol>
          <button
            type="button"
            onClick={dismissDownload}
            className="mt-2 text-xs underline-offset-2 hover:underline"
          >
            {tr("update_dismiss", undefined, "Dismiss")}
          </button>
        </div>
      )}
      {/* Auto-check preference (default on). When off, the launch check is
          skipped entirely — the user drives updates from the button below. */}
      <label className="flex items-start gap-2.5 text-xs">
        <input
          type="checkbox"
          checked={autoCheckEnabled}
          onChange={(e) => setAutoCheckEnabled(e.target.checked)}
          className="mt-0.5 h-3.5 w-3.5 accent-[var(--color-accent)]"
        />
        <span>
          <span className="font-medium text-[var(--color-text)]">
            {tr(
              "update_autocheck_label",
              undefined,
              "Check for updates automatically",
            )}
          </span>
          <span className="block text-[var(--color-muted)]">
            {tr(
              "update_autocheck_hint",
              undefined,
              "Looks for a new release on launch (at most once a day) and notifies you if one is available.",
            )}
          </span>
        </span>
      </label>
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
        {canDownload && mobile && (
          <button
            type="button"
            onClick={handleDownload}
            disabled={busy}
            className="inline-flex items-center gap-1.5 rounded-md bg-[var(--color-accent)] px-3 py-1.5 text-xs font-medium text-[var(--color-accent-contrast)] hover:opacity-90 disabled:opacity-50"
          >
            <ExternalLink size={12} />
            {hasPlatformBundle
              ? tr(
                  "update_open_apk_button",
                  { version: resultForNotes?.latest_version ?? "" },
                  "Get v{version} APK in browser",
                )
              : tr("update_open_release_page", undefined, "Open download page")}
          </button>
        )}
        {canDownload && !mobile && hasPlatformBundle && (
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
        {canDownload && !mobile && !hasPlatformBundle && (
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
  tr: (
    key: string,
    vars?: Record<string, string | number>,
    fallback?: string,
  ) => string,
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
        <span>
          {tr(
            "update_check_error",
            { message: phase.message },
            `Couldn't check for updates: ${phase.message}`,
          )}
        </span>
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
          {
            latest: phase.result.latest_version,
            current: phase.result.current_version,
          },
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
      <span>
        {tr(
          "update_download_error",
          { message: phase.message },
          `Download failed: ${phase.message}`,
        )}
      </span>
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
      <div className="mb-1 flex items-center justify-between text-xs uppercase tracking-wide text-[var(--color-muted)]">
        <span>
          {tr("whats_new_in", { version }, `What's new in v${version}`)}
        </span>
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
