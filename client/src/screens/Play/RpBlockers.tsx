// Play's side of the Remote Play blockers (state/rpBlockers.ts): the question when a start
// finds one on, the phase while one is paused, the hint under a stream error it may have
// caused, and the setting to always pause them.

import { useNavigate } from "react-router";
import { PauseCircle } from "lucide-react";

import { Button, Callout, Spinner, Toggle } from "../../components";
import { hostOf } from "../../lib/addr";
import type { StreamError } from "../../lib/remotePlayStream";
import { useTr } from "../../state/lang";
import {
  blockerNames,
  dropHeld,
  pauseHeldAndStart,
  startHeldAnyway,
  useRpBlockers,
} from "../../state/rpBlockers";
import { blockerById } from "../../lib/rpBlockers";
import { payloadCardLink } from "../Payloads/payloadTabs";

type Tr = ReturnType<typeof useTr>;

const same = (a: string | null | undefined, b: string | null | undefined) =>
  !!a && !!b && hostOf(a.trim()) === hostOf(b.trim());

/** Errors a blocker can be behind: the console never sent a picture, or it still holds the
 *  session the blocked stream left (Hush's "the console keeps believing the previous session is
 *  still finishing"). Not the ones with a cause of their own (pairing, rest mode, network). */
export function blockerCanCause(code: string): boolean {
  return ["busy", "lost", "other", "video_refused", "decode"].includes(code);
}

function reasons(tr: Tr, ids: readonly string[]): string {
  return ids
    .map((id) => blockerById(id))
    .filter((b) => !!b)
    .map((b) => tr(b!.reasonKey, undefined, b!.reason))
    .join(" ");
}

/** Above the picture: the question when a start found a blocker on, a warning while a stream
 *  runs with one on, and the cause under an error one may have caused. */
export function RpBlockerNotice({
  host,
  error,
  running,
}: {
  host: string | null;
  error: StreamError | null;
  running: boolean;
}) {
  const held = useRpBlockers((s) => (same(s.held?.host, host) ? s.held : null));
  const autoPause = useRpBlockers((s) => s.autoPause);
  const onAtStart = useRpBlockers((s) => (host ? s.onAtStart[hostOf(host.trim())] : undefined));
  const paused = useRpBlockers((s) => (host ? s.paused[hostOf(host.trim())] : undefined));
  if (!host) return null;
  return (
    <RpBlockerNoticeView
      heldIds={held?.ids ?? null}
      autoPause={autoPause}
      onAtStart={onAtStart ?? []}
      paused={paused ?? []}
      error={error}
      running={running}
    />
  );
}

/** What RpBlockerNotice shows, from the blockers' state for one console. */
export function RpBlockerNoticeView({
  heldIds,
  autoPause,
  onAtStart,
  paused,
  error,
  running,
}: {
  /** A start held for the user's choice: the blockers it found. */
  heldIds: readonly string[] | null;
  autoPause: boolean;
  onAtStart: readonly string[];
  paused: readonly string[];
  error: StreamError | null;
  running: boolean;
}) {
  const tr = useTr();
  const navigate = useNavigate();
  const setAutoPause = useRpBlockers((s) => s.setAutoPause);

  if (heldIds) {
    const names = blockerNames(heldIds);
    return (
      <Callout
        tone="warn"
        title={reasons(tr, heldIds)}
        onDismiss={dropHeld}
        action={
          <div className="flex flex-wrap gap-2">
            <Button size="sm" variant="primary" leftIcon={<PauseCircle size={14} />} onClick={() => void pauseHeldAndStart()} data-testid="rp-blocker-pause">
              {tr("rp_blocker_pause", { name: names }, `Pause ${names} while I play`)}
            </Button>
            <Button size="sm" variant="ghost" onClick={() => void startHeldAnyway()} data-testid="rp-blocker-anyway">
              {tr("rp_blocker_anyway", undefined, "Start anyway")}
            </Button>
          </div>
        }
      >
        <div className="space-y-2">
          <p>
            {tr(
              "rp_blocker_body",
              { name: names },
              `${names} turns off the console's video encoder, and Remote Play needs it. Paused, ${names} goes back on when the stream ends.`,
            )}
          </p>
          <Toggle
            checked={autoPause}
            onChange={setAutoPause}
            label={tr("rp_blocker_always", undefined, "Always pause Hush for Remote Play")}
          />
        </div>
      </Callout>
    );
  }

  const on = onAtStart.filter((id) => !paused.includes(id));
  if (error && onAtStart.length && blockerCanCause(error.code)) {
    const names = blockerNames(onAtStart);
    return (
      <Callout
        tone="warn"
        title={tr("rp_blocker_cause", { name: names }, `${names} was on when this stream started: it is the likely cause.`)}
        action={
          <Button size="sm" variant="secondary" onClick={() => navigate(payloadCardLink(onAtStart[0]))}>
            {tr("rp_blocker_open_card", { name: names }, `Open ${names}`)}
          </Button>
        }
      >
        {tr(
          "rp_blocker_cause_body",
          { name: names },
          `A session ${names} blocked can keep the PS5 busy until it restarts. Restart the console, then start again and let Play pause ${names}.`,
        )}
      </Callout>
    );
  }
  if (running && on.length) {
    const names = blockerNames(on);
    return (
      <Callout tone="warn" title={reasons(tr, on)}>
        {tr(
          "rp_blocker_running_body",
          { name: names },
          `This stream may have no picture. Stop it, start again and choose Pause ${names} while I play.`,
        )}
      </Callout>
    );
  }
  return null;
}

/** Over the picture while a blocker is paused before the stream starts. */
export function RpBlockerPhase({ host }: { host: string | null }) {
  const pausing = useRpBlockers((s) => (same(s.pausing?.host, host) ? s.pausing : null));
  return pausing ? <RpBlockerPhaseView ids={pausing.ids} /> : null;
}

export function RpBlockerPhaseView({ ids }: { ids: readonly string[] }) {
  const tr = useTr();
  const names = blockerNames(ids);
  return (
    <div
      className="absolute inset-0 z-20 flex items-center justify-center gap-2 bg-black/60 text-sm text-white/85"
      data-testid="rp-blocker-pausing"
    >
      <Spinner size={16} tone="inherit" />
      {tr("rp_phase_pausing", { name: names }, `Pausing ${names} (about 20 s)…`)}
    </div>
  );
}

/** The player's setting: pause blockers without asking. */
export function RpBlockerSetting() {
  const tr = useTr();
  const autoPause = useRpBlockers((s) => s.autoPause);
  const setAutoPause = useRpBlockers((s) => s.setAutoPause);
  return (
    <Toggle
      checked={autoPause}
      onChange={setAutoPause}
      label={tr("rp_blocker_always", undefined, "Always pause Hush for Remote Play")}
      hint={tr(
        "rp_blocker_always_hint",
        undefined,
        "Hush turns off the video encoder Remote Play needs. With this on, Play pauses it before a stream (about 20 s) and turns it back on afterwards, without asking.",
      )}
    />
  );
}
