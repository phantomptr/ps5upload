import { useCallback, useEffect, useRef, useState } from "react";
import { Activity, Gauge, Loader2, Square, Trash2 } from "lucide-react";

import { amprTrace, type TraceStatus } from "../../api/amprTrace";
import { Badge, Button, Card } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import { formatBytes } from "../../lib/format";
import { formatDate } from "../../lib/formatDate";
import { useTr } from "../../state/lang";
import { pushNotification } from "../../state/notifications";
import { humanizeCaught } from "../../lib/humanizeError";

/**
 * Measure how the game reads its files, for an LZ4 image that loads the way it plays: the
 * debug ampr_emu goes into the game's fakelib2 on the console, the person plays, each session
 * is collected (by itself when the game closes while this page is open), and the profile is
 * built from them. Convert's LZ4 option picks the profile up for this title.
 */
export function TraceCard({
  titleId,
  host,
  gamePath,
  consoleName,
  running,
}: {
  titleId: string;
  /** The connected console (bare host), or null when none answers. */
  host: string | null;
  /** The game's folder on that console, or null when it is not a folder there. */
  gamePath: string | null;
  consoleName: string;
  /** The game is running on that console now. */
  running: boolean;
}) {
  const tr = useTr();
  const { confirm, dialog } = useConfirm();
  const [status, setStatus] = useState<TraceStatus | null>(null);
  const [busy, setBusy] = useState<"start" | "stop" | "collect" | "generate" | "forget" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const load = useCallback(() => {
    amprTrace
      .status(titleId)
      .then((s) => {
        // An engine without this route (or a fake one) answers without the fields: treat
        // that as nothing recorded rather than crashing the page.
        setStatus(s && s.state ? s : null);
        setError(null);
      })
      .catch((e) => setError(humanizeCaught(e)));
  }, [titleId]);
  useEffect(load, [load]);

  /** Runs one action; false when it failed (the card shows the error). */
  const run = async (
    what: NonNullable<typeof busy>,
    go: () => Promise<TraceStatus>,
  ): Promise<boolean> => {
    setBusy(what);
    setError(null);
    try {
      const r = await go();
      setStatus(r && r.state ? r : null);
      return true;
    } catch (e) {
      setError(humanizeCaught(e));
      return false;
    } finally {
      setBusy(null);
    }
  };

  // A session ends when the game closes: collect it without being asked.
  const wasRunning = useRef(running);
  useEffect(() => {
    const tracing = !!status?.state?.tracing && status.state.host === host;
    if (wasRunning.current && !running && tracing && host && gamePath && !busy) {
      void run("collect", () => amprTrace.collect(host, gamePath, titleId)).then((ok) => {
        // A failed collect shows its error on the card; saying "recorded" would be untrue.
        if (!ok) return;
        pushNotification("info", tr("trace_collected", undefined, "Play session recorded"), {
          body: tr("trace_collected_body", undefined, "Build the profile when you have played enough, or keep playing."),
        });
      });
    }
    wasRunning.current = running;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [running]);

  const tracing = !!status?.state?.tracing;
  const sessions = status?.sessions ?? [];
  const profile = status?.profile ?? null;
  const canAct = !!host && !!gamePath;
  const sameConsole = !status?.state?.host || status.state.host === host;

  return (
    <Card>
      {dialog}
      <h2 className="mb-2 flex items-center gap-3 text-[1.0625rem] font-semibold tracking-[-0.01em]">
        <span className="grid h-8 w-8 shrink-0 place-items-center rounded-full bg-[var(--color-accent-soft)] text-[var(--color-accent-bright)]">
          <Gauge size={15} aria-hidden />
        </span>
        {tr("trace_title", undefined, "Measure how it plays")}
        {tracing && (
          <Badge tone="accent" size="sm">
            <Activity size={11} className="me-1 animate-pulse" aria-hidden />
            {tr("trace_measuring", undefined, "Measuring")}
          </Badge>
        )}
      </h2>
      <p className="text-sm text-[var(--color-muted)]">
        {tr(
          "trace_what",
          undefined,
          "For an LZ4 game image that loads the way the game plays. A measuring build of ampr_emu records every file the game reads while you play; a profile built from those sessions picks block sizes, layouts and cache sizes per file instead of one guess for all. Convert uses it when you make an LZ4 image of this game.",
        )}
      </p>
      {status && (
        <div className="mt-3 flex flex-col gap-2 text-sm" data-testid="trace-state">
          {tracing && (
            <p>
              {tr(
                "trace_playing_hint",
                { name: status?.state?.host === host ? consoleName : (status?.state?.host ?? "") },
                "Measuring on {name}: start the game, load a save, visit a few areas, then quit it from the console. Each session is recorded when the game closes while this page is open, or with Record session.",
              )}
            </p>
          )}
          <p className="text-[var(--color-muted)]">
            {sessions.length === 0
              ? tr("trace_no_sessions", undefined, "No play session recorded yet.")
              : tr(
                  "trace_sessions",
                  { n: sessions.length, size: formatBytes(sessions.reduce((a, s) => a + s.journal_bytes, 0)) },
                  "{n} play sessions recorded ({size} of reads).",
                )}
          </p>
          {profile && (
            <p data-testid="trace-profile">
              {tr(
                "trace_profile",
                {
                  n: profile.sessions,
                  date: formatDate(new Date(profile.generated_at * 1000)),
                  files: profile.summary?.files_packed ?? 0,
                  cache: formatBytes(profile.summary?.decoded_cache_bytes ?? 0),
                  workers: profile.summary?.runtime_workers ?? 0,
                },
                "Profile built {date} from {n} sessions: {files} files packed, {cache} decoded cache, {workers} workers. Workshop uses it for this game's LZ4 image.",
              )}
            </p>
          )}
        </div>
      )}
      {error && (
        <p className="mt-2 text-xs text-[var(--color-bad)]" role="alert">
          {error}
        </p>
      )}
      <div className="mt-3 flex flex-wrap gap-2">
        {!tracing ? (
          <Button
            size="sm"
            variant="primary"
            leftIcon={busy === "start" ? <Loader2 size={13} className="animate-spin" /> : <Activity size={13} />}
            disabled={!canAct || busy !== null}
            title={
              canAct
                ? undefined
                : tr("trace_needs_folder", undefined, "Needs the game as a folder on a connected console.")
            }
            onClick={() => host && gamePath && void run("start", () => amprTrace.start(host, gamePath, titleId))}
          >
            {tr("trace_start", undefined, "Start measuring")}
          </Button>
        ) : (
          <>
            <Button
              size="sm"
              variant="secondary"
              leftIcon={busy === "collect" ? <Loader2 size={13} className="animate-spin" /> : undefined}
              disabled={!canAct || !sameConsole || busy !== null || running}
              title={running ? tr("trace_quit_first", undefined, "Quit the game first: the session is written when it closes.") : undefined}
              onClick={() => host && gamePath && void run("collect", () => amprTrace.collect(host, gamePath, titleId))}
            >
              {tr("trace_collect", undefined, "Record session")}
            </Button>
            <Button
              size="sm"
              variant="danger"
              leftIcon={busy === "stop" ? <Loader2 size={13} className="animate-spin" /> : <Square size={13} />}
              disabled={!canAct || !sameConsole || busy !== null}
              onClick={() => host && gamePath && void run("stop", () => amprTrace.stop(host, gamePath, titleId))}
            >
              {tr("trace_stop", undefined, "Stop measuring")}
            </Button>
          </>
        )}
        {sessions.length > 0 && (
          <Button
            size="sm"
            variant={profile ? "secondary" : "primary"}
            leftIcon={busy === "generate" ? <Loader2 size={13} className="animate-spin" /> : <Gauge size={13} />}
            disabled={busy !== null}
            onClick={() => void run("generate", () => amprTrace.generate(titleId))}
          >
            {profile ? tr("trace_rebuild", undefined, "Rebuild profile") : tr("trace_build", undefined, "Build profile")}
          </Button>
        )}
        {(sessions.length > 0 || profile) && (
          <Button
            size="sm"
            variant="ghost"
            leftIcon={<Trash2 size={13} />}
            disabled={busy !== null}
            onClick={async () => {
              const ok = await confirm({
                title: tr("trace_forget_title", undefined, "Forget the sessions and profile?"),
                message: tr(
                  "trace_forget_body",
                  undefined,
                  "The recorded sessions and the profile built from them are deleted from this computer. Nothing on the console changes; stop measuring to remove the measuring build there.",
                ),
                confirmLabel: tr("trace_forget", undefined, "Forget"),
                destructive: true,
              });
              if (ok) void run("forget", () => amprTrace.forget(titleId));
            }}
          >
            {tr("trace_forget", undefined, "Forget")}
          </Button>
        )}
      </div>
      {tracing && (
        <p className="mt-2 text-xs text-[var(--color-muted)]">
          {tr(
            "trace_slower",
            undefined,
            "The measuring build is slower than the normal one: stop measuring once the profile is built.",
          )}
        </p>
      )}
    </Card>
  );
}
