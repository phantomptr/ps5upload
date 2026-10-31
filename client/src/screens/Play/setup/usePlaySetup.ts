// The live facts behind Play's setup checklist (playSetup.ts): the connection, the console's
// Remote Play readiness, the signed-in user's PSN state, this app's pairing keys and the user's
// own ticks. Re-read every few seconds while a screen shows it, so steps tick by themselves.

import { useCallback, useEffect, useState } from "react";

import {
  profileInfo,
  profileNpState,
  remoteplayReadiness,
  type NpState,
  type ProfileInfo,
  type RemotePlayReadiness,
} from "../../../api/ps5";
import { mgmtAddr, transferAddr } from "../../../lib/addr";
import { blockerCtx, detectBlockers } from "../../../lib/rpBlockers";
import { useDocumentVisible } from "../../../lib/visibility";
import { useConnectionStore } from "../../../state/connection";
import { usePlaySetupMarks } from "../../../state/playSetup";
import { useRosterStore } from "../../../state/roster";
import { npBackupTarget } from "../../Accounts/npSignIn";
import { nextStep, playReady, setupProgress, setupStates } from "./playSetup";

/** How often the console is asked again while setting up. */
const POLL_MS = 5000;

export function usePlaySetup(opts: { poll?: boolean } = {}) {
  const poll = opts.poll ?? true;
  const host = useConnectionStore((s) => s.host)?.trim() ?? "";
  const connected = useConnectionStore((s) => s.engineStatus === "up" && s.payloadStatus === "up");
  const profile = useRosterStore((s) => s.profiles.find((p) => p.host === host) ?? null);
  const paired = !!(profile?.wake_regist_key && profile?.wake_rp_key);
  const restOk = usePlaySetupMarks((s) => !!s.rest[host]);
  const played = usePlaySetupMarks((s) => !!s.played[host]);
  const setRest = usePlaySetupMarks((s) => s.setRest);
  const visible = useDocumentVisible();

  const [readiness, setReadiness] = useState<RemotePlayReadiness | null>(null);
  const [info, setInfo] = useState<ProfileInfo | null>(null);
  const [np, setNp] = useState<NpState | null>(null);
  const [blockers, setBlockers] = useState<string[] | null>(null);

  const refresh = useCallback(async () => {
    if (!host || !connected) return;
    const [r, i, b] = await Promise.all([
      remoteplayReadiness(transferAddr(host)).catch(() => null),
      profileInfo(mgmtAddr(host)).catch(() => null),
      detectBlockers(blockerCtx(host)).catch(() => null),
    ]);
    setReadiness(r && r.registry_err === 0 ? r : null);
    setInfo(i);
    setBlockers(b ? b.map((x) => x.id) : null);
    const t = npBackupTarget(i);
    setNp(t ? await profileNpState(t.slot, t.uid, mgmtAddr(host)).catch(() => null) : null);
  }, [host, connected]);

  // A different console starts from nothing.
  useEffect(() => {
    setReadiness(null);
    setInfo(null);
    setNp(null);
    setBlockers(null);
  }, [host]);

  useEffect(() => {
    if (!poll || !visible || !connected || !host) return;
    void refresh();
    const t = window.setInterval(() => void refresh(), POLL_MS);
    return () => window.clearInterval(t);
  }, [poll, visible, connected, host, refresh]);

  const states = setupStates({ connected, readiness, np, paired, restOk, played, blockers });
  return {
    host,
    connected,
    readiness,
    info,
    states,
    next: nextStep(states),
    progress: setupProgress(states),
    ready: playReady(states),
    profileId: profile?.id ?? null,
    blockers,
    refresh,
    setRestDone: (on: boolean) => setRest(host, on),
    /** The readiness after the app changed a setting (it answers with the re-read value). */
    setReadiness,
  };
}
