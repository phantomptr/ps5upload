// A console that sleeps, is off, or lost its helper (8.2.0): what Mesh Drive says and offers. The
// engine stops touching a console that is away, keeps every change made on this device, and syncs
// again within a round of the console answering. "Your folders" is read from the engine alone, so
// it shows while the console cannot be reached at all.

import { useHelperReplaceGuard } from "../Connection/useHelperReplaceGuard";
import { useEffect, useState } from "react";
import { FolderSync, Power, Send } from "lucide-react";

import { Badge, Button } from "../../components";
import { fetchLocalFolders, type DriveStatus, type LocalFolder } from "../../api/meshDrive";
import { hostOf } from "../../lib/addr";
import { useConsoleWake } from "../../lib/useConsoleWake";
import { useTr, type Translator } from "../../state/lang";
import { useRosterStore } from "../../state/roster";
import { sendHelperTo } from "../../state/helperSendRuntime";
import { agoText } from "../Devices";
import { driveDetailText } from "./driveDetail";

export type AwayWhy = "rest" | "unreachable" | "helper";

/** The console's name as this app knows it, or its address. */
export function useConsoleName(console: string): string {
  const profiles = useRosterStore((s) => s.profiles);
  const key = hostOf(console) || console;
  return profiles.find((p) => (hostOf(p.host) || p.host) === key)?.name || console;
}

/** The badge and the line for a folder whose console is away. */
export function awayText(
  tr: Translator,
  s: Pick<DriveStatus, "away" | "away_since">,
  name: string,
  now: number,
): { short: string; long: string } {
  const since = s.away_since ? agoText(tr, now - s.away_since) : "";
  const tail = since ? ` ${tr("drive_away_since", { ago: since }, `(away since ${since})`)}` : "";
  switch (s.away) {
    case "rest":
      return {
        short: tr("drive_away_rest", undefined, "Console asleep"),
        long:
          tr(
            "drive_away_rest_text",
            { name },
            `${name} is in rest mode. Changes made here are kept and sync when it wakes.`,
          ) + tail,
      };
    case "helper":
      return {
        short: tr("drive_away_helper", undefined, "Helper not running"),
        long:
          tr(
            "drive_away_helper_text",
            { name },
            `${name} is on, but its helper isn't running (it stops in rest mode). Send it and syncing resumes.`,
          ) + tail,
      };
    default:
      return {
        short: tr("drive_away_unreachable", undefined, "Console away"),
        long:
          tr(
            "drive_away_unreachable_text",
            { name },
            `${name} doesn't answer (off, or on another network). Changes made here are kept and sync when it's back.`,
          ) + tail,
      };
  }
}

/** The one thing to do about it: wake the console, or send its helper. */
export function AwayActions({ console, why }: { console: string; why: AwayWhy }) {
  const tr = useTr();
  const host = hostOf(console) || console;
  const wake = useConsoleWake(host);
  const [sending, setSending] = useState(false);
  const { guard, dialog } = useHelperReplaceGuard();
  if (why === "helper") {
    return (
      <>
        {dialog}
        <Button
          size="sm"
          variant="secondary"
          leftIcon={<Send size={14} />}
          loading={sending}
          onClick={() => {
            setSending(true);
            void guard(host)
              .then((ok) => (ok ? sendHelperTo(host, tr) : undefined))
              .finally(() => setSending(false));
          }}
        >
          {tr("drive_send_helper", undefined, "Send helper")}
        </Button>
      </>
    );
  }
  if (!wake.canWake) return null;
  return (
    <span className="inline-flex flex-col items-start gap-1">
      <Button size="sm" variant="secondary" leftIcon={<Power size={14} />} loading={wake.busy} onClick={() => void wake.wake()}>
        {tr("drive_wake_console", undefined, "Wake console")}
      </Button>
      {wake.error && <span className="text-xs text-[var(--color-bad)]">{wake.error}</span>}
    </span>
  );
}

function FolderRow({ f, now }: { f: LocalFolder; now: number }) {
  const tr = useTr();
  const name = useConsoleName(f.console);
  const why = (f.status.away || "") as AwayWhy | "";
  const away = why ? awayText(tr, f.status, name, now) : null;
  const short =
    away?.short ??
    (f.status.state === "idle"
      ? tr("drive_badge_synced", undefined, "In sync")
      : f.status.state === "error"
        ? tr("drive_badge_error", undefined, "Problem")
        : tr("drive_starting", undefined, "Starting…"));
  const tone = away ? "warn" : f.status.state === "idle" ? "good" : f.status.state === "error" ? "bad" : "neutral";
  const line = away?.long || driveDetailText(tr, f.status.detail_code, f.status.detail_args, f.status.detail);
  return (
    <li className="flex min-w-0 flex-col gap-2 py-3 sm:flex-row sm:items-start">
      <div className="flex min-w-0 flex-1 items-start gap-3">
        <FolderSync size={18} className="mt-0.5 shrink-0 text-[var(--color-accent-bright)]" aria-hidden />
        <div className="min-w-0 flex-1">
          <p className="flex flex-wrap items-center gap-2">
            <span className="truncate font-medium">{f.name || f.drive}</span>
            <Badge tone={tone} dot>
              {short}
            </Badge>
          </p>
          <p className="truncate text-xs text-[var(--color-muted)]" title={f.path}>
            {tr("drive_folder_on", { path: f.path, name }, `${f.path} · home on ${name}`)}
          </p>
          {line && <p className="mt-1 text-xs text-[var(--color-warn)]">{line}</p>}
        </div>
      </div>
      {why && <AwayActions console={f.console} why={why} />}
    </li>
  );
}

/** This device's folders, from the engine alone: shown while the console cannot be reached. */
export function AwayFolders() {
  const tr = useTr();
  const [folders, setFolders] = useState<LocalFolder[] | null>(null);
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    let live = true;
    const load = () =>
      void fetchLocalFolders().then((f) => {
        if (!live) return;
        setFolders(f);
        setNow(Date.now());
      });
    load();
    const id = window.setInterval(load, 5_000);
    return () => {
      live = false;
      window.clearInterval(id);
    };
  }, []);
  if (!folders?.length) return null;
  return (
    <section className="rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface)] p-4">
      <h3 className="text-sm font-semibold">{tr("drive_your_folders", undefined, "Your folders")}</h3>
      <p className="text-xs text-[var(--color-muted)]">
        {tr(
          "drive_your_folders_hint",
          undefined,
          "This device keeps these folders. Nothing is lost while a console is away: changes made here wait and sync when it's back.",
        )}
      </p>
      <ul className="divide-y divide-[var(--color-border)]">
        {folders.map((f) => (
          <FolderRow key={f.member} f={f} now={now} />
        ))}
      </ul>
    </section>
  );
}
