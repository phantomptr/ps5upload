import { useCallback, useEffect, useRef, useState } from "react";
import { Bluetooth, ExternalLink, RefreshCw, Search } from "lucide-react";

import { Badge, Button, Callout, Toggle } from "../../../components";
import { useConfirm } from "../../../components/ConfirmDialog";
import {
  hbAct,
  hbOverview,
  hbSaved,
  hbScanned,
  type HbAction,
  type HbOverview,
  type HbSavedDevice,
  type HbScannedDevice,
  type HbStatus,
} from "../../../api/hearbridge";
import { withPort } from "../../../lib/addr";
import { openExternalUrl } from "../../../lib/openExternalUrl";
import { usePoll } from "../../../lib/usePoll";
import { useDocumentVisible } from "../../../lib/visibility";
import { useTr } from "../../../state/lang";
import { log } from "../../../state/logs";
import { humanizeCaught } from "../../../lib/humanizeError";

/** HearBridge's status is re-read this often while its card is on screen. Each read
 *  counts as its page being open on the console side only for the volume re-query, so
 *  keep it unhurried. */
const POLL_MS = 5000;
/** …and this often while the card is collapsed. */
const POLL_SLOW_MS = 15_000;
/** HearBridge's scan runs about 12 s (its own page says so). */
const SCAN_MS = 13_000;

export const HB_DEFAULT_PORT = 8090;

/** HearBridge's status on `host`, kept fresh while `active`. `null` until first read. */
export function useHearBridge(host: string, active: boolean, fast = false) {
  const visible = useDocumentVisible();
  const [view, setView] = useState<HbOverview | null>(null);
  const gen = useRef(0);
  useEffect(() => {
    gen.current++;
    setView(null);
  }, [host]);
  const refresh = useCallback(async () => {
    if (!host.trim()) return;
    const g = gen.current;
    try {
      const v = await hbOverview(host);
      if (g === gen.current) setView(v);
    } catch (e) {
      if (g === gen.current) setView({ present: false, error: humanizeCaught(e) });
    }
  }, [host]);
  usePoll(refresh, fast ? POLL_MS : POLL_SLOW_MS, { host, enabled: active && visible && !!host.trim() });
  /** Use the status HearBridge sent back with a change, so the panel doesn't wait a poll. */
  const apply = useCallback((s: HbStatus | null) => {
    if (!s) return;
    setView((v) => (v && v.present ? { ...v, status: s } : v));
  }, []);
  return { view, refresh, apply };
}

/** The card's one-line summary: state, headset, battery, link, latency, codec, volume, game. */
export function HbSummary({ view }: { view: HbOverview | null }) {
  const tr = useTr();
  if (!view) return <>{tr("hb_checking", undefined, "Reading HearBridge's status…")}</>;
  if (!view.present) return <>{tr("hb_no_answer", undefined, "HearBridge didn't answer")}</>;
  return <>{hbSummaryParts(view.status, tr).join(" · ")}</>;
}

type Tr = ReturnType<typeof useTr>;

export function hbStateLabel(state: string, tr: Tr): string {
  switch (state) {
    case "streaming":
      return tr("hb_state_streaming", undefined, "Streaming");
    case "connecting":
      return tr("hb_state_connecting", undefined, "Connecting");
    case "reconnecting":
      return tr("hb_state_reconnecting", undefined, "Reconnecting");
    case "scanning":
      return tr("hb_state_scanning", undefined, "Scanning");
    case "disconnected":
      return tr("hb_state_waiting", undefined, "Waiting for a headset");
    case "error":
      return tr("hb_state_error", undefined, "Error");
    default:
      return state;
  }
}

export function hbSummaryParts(s: HbStatus, tr: Tr): string[] {
  const parts: string[] = [];
  if (s.state) parts.push(hbStateLabel(s.state, tr));
  if (s.device) parts.push(s.device);
  if (s.batteryPct !== null) parts.push(tr("hb_battery", { pct: s.batteryPct }, `Battery ${s.batteryPct}%`));
  if (s.linkScore !== null && s.state === "streaming") {
    parts.push(tr("hb_link", { score: s.linkScore }, `Link ${s.linkScore}%`));
  }
  const ms = s.latencyEstimateMs ?? s.latencyTargetMs;
  if (ms !== null && s.state === "streaming") parts.push(tr("hb_latency", { ms }, `${ms} ms delay`));
  if (s.codec && s.state === "streaming") parts.push(s.codec);
  if (s.muted) parts.push(tr("hb_muted", undefined, "Muted"));
  else if (s.gainPct !== null) parts.push(tr("hb_volume", { pct: s.gainPct }, `Volume ${s.gainPct}%`));
  if (s.gameProfile && s.gameName) parts.push(tr("hb_game", { name: s.gameName }, `Profile: ${s.gameName}`));
  return parts;
}

/** HearBridge's page on the console (pairing, EQ, backups and the rest live there). */
export function hbPageUrl(host: string, port: number | null | undefined): string {
  return `http://${withPort(host.trim(), port ?? HB_DEFAULT_PORT)}/`;
}

/** Manage: the everyday controls. Everything else stays on HearBridge's own page. */
export function HearBridgePanel({
  host,
  view,
  apply,
  refresh,
}: {
  host: string;
  view: HbOverview | null;
  apply: (s: HbStatus | null) => void;
  refresh: () => Promise<void>;
}) {
  const tr = useTr();
  const { confirm, dialog } = useConfirm();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState<HbSavedDevice[] | null>(null);
  const [scanned, setScanned] = useState<HbScannedDevice[] | null>(null);
  const [scanning, setScanning] = useState(false);
  // Slider values while dragging; sent on release so a drag is one change, not dozens.
  const [gainDraft, setGainDraft] = useState<number | null>(null);
  const [latDraft, setLatDraft] = useState<number | null>(null);
  const present = view?.present === true;
  const port = view && view.present ? view.port : HB_DEFAULT_PORT;
  const s = view && view.present ? view.status : null;

  const loadSaved = useCallback(async () => {
    try {
      setSaved(await hbSaved(host));
    } catch {
      setSaved(null);
    }
  }, [host]);
  useEffect(() => {
    if (present) void loadSaved();
  }, [present, loadSaved]);

  /** Make one change; false when HearBridge didn't take it (the error is shown). */
  const run = async (a: HbAction): Promise<boolean> => {
    setBusy(true);
    setError(null);
    try {
      apply(await hbAct(host, a));
      log.info("hearbridge", `${a.action} on ${host}`);
      await refresh();
      return true;
    } catch (e) {
      setError(humanizeCaught(e));
      return false;
    } finally {
      setBusy(false);
    }
  };

  const scan = async () => {
    if (s?.state === "streaming") {
      const ok = await confirm({
        title: tr("hb_scan_confirm_title", undefined, "Scan for headsets?"),
        message: tr(
          "hb_scan_confirm_body",
          undefined,
          "Scanning stops the headset's audio while it runs (about 12 seconds).",
        ),
        confirmLabel: tr("hb_scan", undefined, "Scan for headsets"),
      });
      if (!ok) return;
    }
    setScanning(true);
    setScanned(null);
    if (!(await run({ action: "scan" }))) {
      setScanning(false);
      return;
    }
    window.setTimeout(() => {
      hbScanned(host)
        .then(setScanned)
        .catch(() => setScanned([]))
        .finally(() => setScanning(false));
    }, SCAN_MS);
  };

  const openPage = (
    <Button
      variant="secondary"
      size="sm"
      leftIcon={<ExternalLink size={14} />}
      onClick={() => void openExternalUrl(hbPageUrl(host, port))}
    >
      {tr("hb_open_page", undefined, "Open HearBridge page")}
    </Button>
  );

  if (!view) return null;
  if (!present || !s) {
    return (
      <div className="flex flex-col gap-3">
        <Callout tone="warn" title={tr("hb_no_answer", undefined, "HearBridge didn't answer")}>
          {view.present ? null : view.error}
        </Callout>
        <div>{openPage}</div>
      </div>
    );
  }

  const gain = gainDraft ?? s.gainPct ?? 100;
  const lat = latDraft ?? s.latencyTargetMs ?? 120;

  return (
    <div className="flex flex-col gap-4 text-sm">
      {error && (
        <Callout tone="error" title={tr("hb_action_failed", undefined, "HearBridge didn't take that change")}>
          {error}
        </Callout>
      )}

      <section className="flex flex-col gap-2">
        <div className="flex flex-wrap items-center gap-3">
          <label className="flex min-w-[min(100%,18rem)] flex-1 items-center gap-3">
            <span className="w-24 shrink-0">{tr("hb_volume_label", undefined, "Volume")}</span>
            <input
              type="range"
              min={0}
              max={500}
              step={10}
              value={gain}
              disabled={busy || s.gainPct === null}
              onChange={(e) => setGainDraft(Number(e.target.value))}
              onPointerUp={() => {
                if (gainDraft !== null) void run({ action: "volume", pct: gainDraft }).finally(() => setGainDraft(null));
              }}
              onKeyUp={() => {
                if (gainDraft !== null) void run({ action: "volume", pct: gainDraft }).finally(() => setGainDraft(null));
              }}
              className="flex-1"
              aria-label={tr("hb_volume_label", undefined, "Volume")}
            />
            <span className="w-14 text-end font-mono tabular-nums">{gain}%</span>
          </label>
          <Toggle
            checked={!!s.muted}
            onChange={(on) => void run({ action: "mute", on })}
            label={tr("hb_mute", undefined, "Mute")}
          />
        </div>
        <div className="flex flex-wrap items-center gap-3">
          <span className="w-24 shrink-0">{tr("hb_latency_label", undefined, "Latency")}</span>
          <Toggle
            checked={!!s.latencyAuto}
            onChange={(auto) => void run({ action: "latency", auto })}
            label={tr("hb_latency_auto", undefined, "Automatic")}
          />
          {!s.latencyAuto && (
            <label className="flex min-w-[min(100%,14rem)] flex-1 items-center gap-3">
              <input
                type="range"
                min={40}
                max={400}
                step={10}
                value={lat}
                disabled={busy}
                onChange={(e) => setLatDraft(Number(e.target.value))}
                onPointerUp={() => {
                  if (latDraft !== null) void run({ action: "latency", ms: latDraft }).finally(() => setLatDraft(null));
                }}
                onKeyUp={() => {
                  if (latDraft !== null) void run({ action: "latency", ms: latDraft }).finally(() => setLatDraft(null));
                }}
                className="flex-1"
                aria-label={tr("hb_latency_label", undefined, "Latency")}
              />
              <span className="w-16 text-end font-mono tabular-nums">{lat} ms</span>
            </label>
          )}
        </div>
        <Toggle
          checked={!!s.night}
          onChange={(on) => void run({ action: "night", on })}
          label={tr("hb_night", undefined, "Night mode")}
          hint={tr("hb_night_hint", undefined, "Quieter explosions, clearer voices.")}
        />
      </section>

      <section className="flex flex-col gap-2">
        <div className="flex flex-wrap items-center gap-2">
          <h4 className="font-medium">{tr("hb_headsets", undefined, "Headsets")}</h4>
          <Button
            variant="secondary"
            size="sm"
            leftIcon={<RefreshCw size={14} />}
            disabled={busy}
            onClick={() => void run({ action: "reconnect" })}
          >
            {tr("hb_reconnect", undefined, "Reconnect")}
          </Button>
          <Button
            variant="secondary"
            size="sm"
            leftIcon={<Search size={14} />}
            loading={scanning}
            disabled={busy || scanning}
            onClick={() => void scan()}
          >
            {tr("hb_scan", undefined, "Scan for headsets")}
          </Button>
        </div>
        {saved && saved.length > 0 && (
          <ul className="flex flex-col gap-1.5">
            {saved.map((d) => (
              <li key={d.addr} className="flex flex-wrap items-center gap-2">
                <Bluetooth size={14} className="text-[var(--color-muted)]" />
                <span>{d.name || d.addr}</span>
                {d.current ? (
                  <Badge tone="good">{tr("hb_current", undefined, "In use")}</Badge>
                ) : (
                  <Button
                    variant="ghost"
                    size="sm"
                    disabled={busy}
                    onClick={() => void run({ action: "select", addr: d.addr })}
                  >
                    {tr("hb_switch", undefined, "Switch to this one")}
                  </Button>
                )}
              </li>
            ))}
          </ul>
        )}
        {saved && saved.length === 0 && (
          <p className="text-xs text-[var(--color-muted)]">
            {tr("hb_saved_empty", undefined, "No saved headsets yet. Scan for one, or pair it on HearBridge's page.")}
          </p>
        )}
        {scanned && (
          <div className="flex flex-col gap-1.5">
            <span className="text-xs text-[var(--color-muted)]">
              {scanned.length
                ? tr("hb_scan_found", undefined, "Found in the last scan:")
                : tr("hb_scan_empty", undefined, "Nothing found. Put the headset in pairing mode and scan again.")}
            </span>
            <ul className="flex flex-col gap-1.5">
              {scanned.map((d) => (
                <li key={`${d.index}-${d.addr}`} className="flex flex-wrap items-center gap-2">
                  <Bluetooth size={14} className="text-[var(--color-muted)]" />
                  <span>{d.name || d.addr}</span>
                  <Button
                    variant="ghost"
                    size="sm"
                    disabled={busy}
                    onClick={() => void run({ action: "select", index: d.index })}
                  >
                    {tr("hb_connect", undefined, "Connect")}
                  </Button>
                </li>
              ))}
            </ul>
          </div>
        )}
      </section>

      {s.events.length > 0 && (
        <section className="flex flex-col gap-1">
          <h4 className="font-medium">{tr("hb_events", undefined, "Recent events")}</h4>
          <ul className="font-mono text-xs text-[var(--color-muted)]">
            {s.events.slice(-8).reverse().map((e, i) => (
              <li key={`${i}-${e}`}>{e}</li>
            ))}
          </ul>
        </section>
      )}

      <section className="flex flex-wrap items-center gap-3">
        {openPage}
        <span className="text-xs text-[var(--color-muted)]">
          {tr(
            "hb_page_hint",
            undefined,
            "Pairing a new headset, the equalizer, backups and every other setting are on HearBridge's own page.",
          )}
        </span>
      </section>
      {dialog}
    </div>
  );
}
