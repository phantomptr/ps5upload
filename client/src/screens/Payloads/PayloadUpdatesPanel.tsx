import { useCallback, useEffect, useState } from "react";
import { Download, RefreshCw, RotateCcw, Upload } from "lucide-react";

import { Badge, Button, Callout, Checkbox, Spinner, Toggle } from "../../components";
import {
  applyPayloadUpdates,
  checkPayloadUpdates,
  payloadUpdatesOverview,
  pmSyncChoice,
  rollbackPayload,
  runPmSync,
  setPayloadUpdateSettings,
  setPmSyncChoice,
  type PayloadUpdateRow,
  type PayloadUpdateSettings,
  type PayloadUpdateState,
  type PmChange,
  type PmSyncChoice,
  type PmSyncReport,
} from "../../api/payloadUpdates";
import { pmOverview } from "../../api/payloadManager";
import { hostOf } from "../../lib/addr";
import { relativeAge } from "../../lib/formatDate";
import { humanizeCaught } from "../../lib/humanizeError";
import { markPayloadUpdatesSeen } from "../../lib/payloadUpdateNotices";
import { useTr, type Translator } from "../../state/lang";
import { pushNotification } from "../../state/notifications";
import { withConsolePrefix } from "../../state/roster";

/** One line for what a Payload Manager sync did. */
export function pmChangeText(c: PmChange, tr: Translator, nameOf: (id: string) => string): string {
  const name = nameOf(c.payload_id);
  switch (c.kind) {
    case "stored":
      return tr("pu_change_stored", { name, version: c.to }, `Stored ${name} ${c.to}`);
    case "unchanged":
      return tr("pu_change_unchanged", { name, version: c.version }, `${name} ${c.version} already there`);
    case "skipped": {
      const reason =
        c.code === "not_downloaded"
          ? tr("pu_skip_not_downloaded", undefined, "not downloaded to this computer yet")
          : c.code === "folder_in_use"
            ? tr(
                "pu_skip_folder",
                undefined,
                "its folder in Payload Manager holds other files; store it once from the Payload Manager tab",
              )
            : c.code === "repo_managed"
              ? tr("pu_skip_repo", undefined, "Payload Manager updates it from its own repository")
              : c.reason;
      return tr("pu_change_skipped", { name, reason }, `Skipped ${name}: ${reason}`);
    }
    case "failed":
      return c.payload_id
        ? tr("pu_change_failed", { name, error: c.error }, `${name} failed: ${c.error}`)
        : c.error;
    case "autoload_added":
      return tr("pu_change_boot_added", { name }, `${name} now loads at boot`);
    case "autoload_renamed":
      return tr("pu_change_boot_renamed", { name }, `${name}'s boot entry now names the new file`);
    case "autoload_removed":
      return tr("pu_change_boot_removed", { name }, `${name} no longer loads at boot`);
    case "removed_old":
      return tr("pu_change_removed_old", { file: c.filename }, `Removed the older copy ${c.filename}`);
  }
}

/** A sync worth listing: the lines that say something changed or went wrong. */
export function visibleChanges(r: PmSyncReport): PmChange[] {
  return r.changes.filter((c) => c.kind !== "unchanged");
}

export interface PayloadUpdatesViewProps {
  tr: Translator;
  settings: PayloadUpdateSettings | null;
  state: PayloadUpdateState | null;
  /** The selected console ("" for none). */
  host: string;
  /** Payload Manager answered on that console (null: not known yet). */
  pmPresent: boolean | null;
  choice: PmSyncChoice | null;
  busy: string | null;
  nowSecs: number;
  onCheck: () => void;
  onUpdate: (id?: string) => void;
  onRollback: (id: string) => void;
  onAuto: (on: boolean) => void;
  onChoice: (id: string, keep: boolean, boot: boolean) => void;
  onSync: () => void;
}

/** "Payload updates" on the Catalog tab: check, update, roll back, and keep Payload Manager in
 *  step. Presentational; `PayloadUpdatesPanel` feeds it. */
export function PayloadUpdatesView(p: PayloadUpdatesViewProps) {
  const { tr, state, settings, busy, choice } = p;
  const rows = state?.rows ?? [];
  const available = rows.filter((r) => r.update_available);
  const pmOn = !!p.host && p.pmPresent === true;
  const report = p.host ? state?.pm_syncs?.[hostOf(p.host)] : undefined;
  const nameOf = (id: string) => rows.find((r) => r.payload_id === id)?.display_name || id;
  const icon = (key: string, el: React.ReactNode) => (busy === key ? <Spinner size={12} tone="inherit" /> : el);

  const row = (r: PayloadUpdateRow) => {
    const keep = !!choice?.ids.includes(r.payload_id);
    const boot = !!choice?.autoload_ids.includes(r.payload_id);
    return (
      <li
        key={r.payload_id}
        data-testid={`pu-row-${r.payload_id}`}
        className="flex flex-wrap items-center gap-2 rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] px-3 py-2 text-sm"
      >
        <div className="min-w-[min(100%,12rem)] flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <span className="font-medium">{r.display_name || r.payload_id}</span>
            <span className="font-mono text-xs text-[var(--color-muted)]">
              {r.installed_version || tr("pu_not_downloaded", undefined, "not downloaded")}
            </span>
            {r.update_available && (
              <Badge tone="warn" variant="outline">
                {tr("pm_update_available", { tag: r.latest_version }, `${r.latest_version} available`)}
              </Badge>
            )}
          </div>
          {r.error && !r.update_available && (
            <div className="truncate text-xs text-[var(--color-muted)]" title={r.error}>
              {r.error}
            </div>
          )}
          {pmOn && (
            <div className="mt-1 flex flex-wrap gap-x-4">
              <Checkbox
                checked={keep}
                disabled={busy !== null}
                onChange={(on) => p.onChoice(r.payload_id, on, on && boot)}
                label={<span className="text-xs">{tr("pu_pm_keep", undefined, "Keep in Payload Manager")}</span>}
              />
              <Checkbox
                checked={boot}
                disabled={busy !== null}
                onChange={(on) => p.onChoice(r.payload_id, keep || on, on)}
                label={<span className="text-xs">{tr("pu_pm_boot", undefined, "Payload Manager loads it at boot")}</span>}
              />
            </div>
          )}
        </div>
        <div className="flex flex-wrap items-center gap-2">
          {r.update_available && (
            <Button
              variant="secondary"
              size="sm"
              leftIcon={icon(`update:${r.payload_id}`, <Download size={12} />)}
              disabled={busy !== null}
              onClick={() => p.onUpdate(r.payload_id)}
            >
              {tr("pm_update", undefined, "Update")}
            </Button>
          )}
          {r.previous_version && (
            <Button
              variant="ghost"
              size="sm"
              leftIcon={icon(`rollback:${r.payload_id}`, <RotateCcw size={12} />)}
              disabled={busy !== null}
              onClick={() => p.onRollback(r.payload_id)}
              title={tr(
                "pu_rollback_tooltip",
                undefined,
                "Puts back the version the last update replaced. Do it again to return.",
              )}
            >
              {tr("pu_rollback", { version: r.previous_version }, `Roll back to ${r.previous_version}`)}
            </Button>
          )}
        </div>
      </li>
    );
  };

  return (
    <section className="surface-panel space-y-3 p-4" aria-labelledby="pu-title" data-testid="payload-updates">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div>
          <h2 id="pu-title" className="text-sm font-semibold">
            {tr("pu_title", undefined, "Payload updates")}
          </h2>
          <p className="text-xs text-[var(--color-muted)]">
            {!state || state.checked_at === 0
              ? tr(
                  "pu_never_checked",
                  undefined,
                  "Not checked yet. A check compares each payload you downloaded with its latest release.",
                )
              : available.length > 0
                ? tr("pu_count", { n: available.length }, `Updates available: ${available.length}`)
                : tr("pu_none", undefined, "Everything is up to date.")}
            {state && state.checked_at > 0 && (
              <>
                {" "}
                {tr(
                  "pu_checked",
                  { when: relativeAge(Math.max(0, p.nowSecs - state.checked_at)) },
                  `Checked ${relativeAge(Math.max(0, p.nowSecs - state.checked_at))}.`,
                )}
              </>
            )}
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Button
            variant="secondary"
            size="sm"
            leftIcon={icon("check", <RefreshCw size={12} />)}
            disabled={busy !== null}
            onClick={p.onCheck}
          >
            {tr("pu_check", undefined, "Check for payload updates")}
          </Button>
          {available.length > 1 && (
            <Button
              variant="primary"
              size="sm"
              leftIcon={icon("update:all", <Download size={12} />)}
              disabled={busy !== null}
              onClick={() => p.onUpdate()}
            >
              {tr("pu_update_all", undefined, "Update all")}
            </Button>
          )}
        </div>
      </div>

      {settings && (
        <Toggle
          checked={settings.auto_update}
          disabled={busy !== null}
          onChange={p.onAuto}
          label={tr("pu_auto_label", undefined, "Keep payloads up to date")}
          hint={tr(
            "pu_auto_hint",
            undefined,
            "Checks when the app starts and once a day, and downloads newer releases. The version each update replaces is kept, so you can roll back.",
          )}
        />
      )}

      {state?.rate_limited && (
        <p className="text-xs text-[var(--color-warn)]">
          {tr(
            "pu_rate_limited",
            undefined,
            "GitHub's limit for checks was reached, so some versions are from the last check. Try again in an hour.",
          )}
        </p>
      )}

      {rows.length > 0 && <ul className="grid gap-1.5">{rows.map(row)}</ul>}

      {p.host && p.pmPresent === false && (
        <Callout tone="info" title={tr("pm_absent_title", undefined, "Payload Manager isn't running on this PS5")}>
          {tr(
            "pu_pm_absent",
            undefined,
            "Updates go to this computer's payloads only. Start Payload Manager on the PS5 to keep its copies and boot list in step.",
          )}
        </Callout>
      )}

      {pmOn && (
        <div className="space-y-2">
          <div className="flex flex-wrap items-center gap-2">
            <Button
              variant="secondary"
              size="sm"
              leftIcon={icon("sync", <Upload size={12} />)}
              disabled={busy !== null || !choice || choice.ids.length === 0}
              onClick={p.onSync}
            >
              {tr("pu_pm_sync", undefined, "Sync with Payload Manager")}
            </Button>
            <p className="min-w-[min(100%,16rem)] flex-1 text-xs text-[var(--color-muted)]">
              {tr(
                "pu_pm_hint",
                undefined,
                "Copies the payloads you keep there into Payload Manager when they change, and adds the ones you mark to its boot list. Nothing else in its list is changed.",
              )}
            </p>
          </div>
          {report && (
            <div className="text-xs" data-testid="pu-sync-report">
              {!report.present ? (
                <p className="text-[var(--color-muted)]">
                  {tr(
                    "pu_pm_absent",
                    undefined,
                    "Updates go to this computer's payloads only. Start Payload Manager on the PS5 to keep its copies and boot list in step.",
                  )}
                </p>
              ) : visibleChanges(report).length === 0 ? (
                <p className="text-[var(--color-muted)]">
                  {report.message ?? tr("pu_pm_in_step", undefined, "Payload Manager already matches.")}
                </p>
              ) : (
                <>
                  <p className="text-[var(--color-muted)]">
                    {tr(
                      "pu_pm_last_sync",
                      { when: relativeAge(Math.max(0, p.nowSecs - report.at)) },
                      `Last sync, ${relativeAge(Math.max(0, p.nowSecs - report.at))}:`,
                    )}
                  </p>
                  <ul className="list-disc ps-5">
                    {visibleChanges(report).map((c, i) => (
                      <li key={`${c.kind}:${c.payload_id}:${i}`}>{pmChangeText(c, tr, nameOf)}</li>
                    ))}
                  </ul>
                </>
              )}
            </div>
          )}
        </div>
      )}
    </section>
  );
}

/** The update section of the Catalog tab, fed from the engine. `onChanged` runs after anything
 *  replaced a downloaded file, so the cards below show the new version. */
export default function PayloadUpdatesPanel({ host, onChanged }: { host: string; onChanged: () => void }) {
  const tr = useTr();
  const [settings, setSettings] = useState<PayloadUpdateSettings | null>(null);
  const [state, setState] = useState<PayloadUpdateState | null>(null);
  const [pmPresent, setPmPresent] = useState<boolean | null>(null);
  const [choice, setChoice] = useState<PmSyncChoice | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  // "Checked 2 minutes ago" counts from when the state was last read.
  const [nowSecs, setNowSecs] = useState(() => Math.floor(Date.now() / 1000));

  const show = useCallback((s: PayloadUpdateState) => {
    setState(s);
    setNowSecs(Math.floor(Date.now() / 1000));
    // What is on screen is not announced again as a notice.
    markPayloadUpdatesSeen(s);
  }, []);

  const reload = useCallback(async () => {
    const v = await payloadUpdatesOverview();
    setSettings(v.settings);
    show(v.state);
  }, [show]);

  useEffect(() => {
    reload().catch(() => undefined);
  }, [reload]);

  useEffect(() => {
    const h = host.trim();
    setPmPresent(null);
    setChoice(null);
    if (!h) return;
    let live = true;
    pmOverview(h)
      .then((v) => live && setPmPresent(v.present))
      .catch(() => live && setPmPresent(false));
    pmSyncChoice(h)
      .then((c) => live && setChoice(c))
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [host]);

  const run = async (key: string, work: () => Promise<void>, failTitle: string) => {
    setBusy(key);
    try {
      await work();
    } catch (e) {
      pushNotification("error", failTitle, { body: humanizeCaught(e) });
    } finally {
      setBusy(null);
    }
  };

  const nameOf = (id: string) => state?.rows.find((r) => r.payload_id === id)?.display_name || id;
  const syncTarget = pmPresent && choice && choice.ids.length > 0 ? host : undefined;

  const onUpdate = (id?: string) =>
    run(
      id ? `update:${id}` : "update:all",
      async () => {
        const r = await applyPayloadUpdates(id ? [id] : undefined, syncTarget);
        for (const u of r.updated.filter((u) => u.updated)) {
          pushNotification(
            "success",
            tr("pu_updated", { name: u.display_name || u.payload_id, version: u.to }, `Updated ${u.display_name} to ${u.to}`),
          );
        }
        for (const e of r.errors) {
          pushNotification("error", tr("pu_update_failed", { name: nameOf(e.payload_id) }, `Couldn't update ${nameOf(e.payload_id)}`), {
            body: e.error,
          });
        }
        if (r.pm_sync && !r.pm_sync.present) {
          pushNotification("info", withConsolePrefix(host, tr("pm_absent_title", undefined, "Payload Manager isn't running on this PS5")));
        }
        onChanged();
        await reload();
      },
      tr("pu_update_failed", { name: id ? nameOf(id) : "" }, `Couldn't update ${id ? nameOf(id) : ""}`),
    );

  return (
    <PayloadUpdatesView
      tr={tr}
      settings={settings}
      state={state}
      host={host}
      pmPresent={pmPresent}
      choice={choice}
      busy={busy}
      nowSecs={nowSecs}
      onCheck={() =>
        void run(
          "check",
          async () => show(await checkPayloadUpdates(true)),
          tr("pu_check_failed", undefined, "Couldn't check for payload updates"),
        )
      }
      onUpdate={(id) => void onUpdate(id)}
      onRollback={(id) =>
        void run(
          `rollback:${id}`,
          async () => {
            const r = await rollbackPayload(id);
            pushNotification(
              "success",
              tr("pu_rolled_back", { name: nameOf(id), version: r.to }, `Rolled ${nameOf(id)} back to ${r.to}`),
            );
            onChanged();
            await reload();
          },
          tr("pu_update_failed", { name: nameOf(id) }, `Couldn't update ${nameOf(id)}`),
        )
      }
      onAuto={(on) =>
        void run(
          "auto",
          async () => setSettings(await setPayloadUpdateSettings({ auto_update: on })),
          tr("pu_setting_failed", undefined, "Couldn't save the setting"),
        )
      }
      onChoice={(id, keep, boot) => {
        const cur = choice ?? { ids: [], autoload_ids: [] };
        const ids = keep ? [...new Set([...cur.ids, id])] : cur.ids.filter((x) => x !== id);
        const autoload_ids = boot
          ? [...new Set([...cur.autoload_ids, id])]
          : cur.autoload_ids.filter((x) => x !== id);
        void run(
          `choice:${id}`,
          async () => setChoice(await setPmSyncChoice(host, { ids, autoload_ids })),
          tr("pu_sync_failed", undefined, "Couldn't sync with Payload Manager"),
        );
      }}
      onSync={() =>
        void run(
          "sync",
          async () => {
            await runPmSync(host);
            await reload();
          },
          withConsolePrefix(host, tr("pu_sync_failed", undefined, "Couldn't sync with Payload Manager")),
        )
      }
    />
  );
}
