import { useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { useShallow } from "zustand/react/shallow";
import {
  ArrowLeftRight,
  LayoutGrid,
  LogIn,
  MonitorPlay,
  Play as PlayIcon,
  Plus,
  Settings2,
  Square,
  Wrench,
} from "lucide-react";

import { AppLogo, Modal } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import { hostOf } from "../../lib/addr";
import { consoleLight, consoleLightColor, type HostPower } from "../../lib/consolePower";
import { useScreenBack } from "../../lib/systemBack";
import { canLeaveApp, leaveApp } from "../../lib/tvNav";
import { useConnectionStore } from "../../state/connection";
import { useTr } from "../../state/lang";
import { useRemotePlayStore } from "../../state/remotePlay";
import { useRosterStore, type PS5Profile } from "../../state/roster";
import { useTvModeStore } from "../../state/tvMode";
import { consoleOnTailnet, useTailnet } from "../../state/useTailnet";
import { useStreamLaunch } from "../Play/game/streamLaunch";
import { leaveAfterAsking, tvCardState, tvFocusTarget, tvPrimary, type TvCardState } from "./tvCard";
import { TvPlaySettings } from "./TvPlaySettings";

/** A big button for a remote: one look, focus ring from the app's TV styles (index.css). */
function TvButton({
  primary = false,
  icon,
  children,
  onClick,
  testId,
}: {
  primary?: boolean;
  icon: React.ReactNode;
  children: React.ReactNode;
  onClick: () => void;
  testId: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      data-testid={testId}
      data-tv-primary={primary ? "" : undefined}
      className={`inline-flex min-h-14 min-w-0 max-w-full items-center justify-center gap-3 rounded-2xl px-6 py-3 text-start text-lg font-semibold transition-colors sm:min-h-16 sm:px-8 sm:text-xl ${
        primary
          ? "bg-[var(--color-accent)] text-[var(--color-accent-contrast)] hover:brightness-110"
          : "border border-[var(--color-border)] bg-[var(--color-surface-3)] text-[var(--color-text)] hover:bg-[var(--color-surface-raised)]"
      }`}
    >
      <span className="shrink-0" aria-hidden>
        {icon}
      </span>
      <span className="min-w-0 break-words">{children}</span>
    </button>
  );
}

/** One line for the console's state, and the colour of its light. */
function stateLine(tr: ReturnType<typeof useTr>, state: TvCardState, playingName: string): string {
  switch (state) {
    case "playing":
      return tr("tv_state_playing", { name: playingName }, `Playing ${playingName}`);
    case "setup":
      return tr("tv_state_setup", undefined, "Not set up for Remote Play yet");
    case "on":
      return tr("tv_state_on", undefined, "On");
    case "rest":
      return tr("tv_state_rest", undefined, "In rest mode. Play wakes it.");
    case "unreachable":
      return tr("tv_state_unreachable", undefined, "Off or not reachable. Play tries to wake it.");
    case "checking":
      return tr("tv_state_checking", undefined, "Looking for it…");
    default:
      return tr("tv_state_none", undefined, "Add your PS5 to play it on this screen.");
  }
}

/** The colour of a console's light (the same lights as the console tabs). */
function lightOf(power: HostPower, payloadUp: boolean): string {
  return consoleLightColor(consoleLight({ payloadStatus: payloadUp ? "up" : "down", power }));
}

/** Pick the console to play (TV mode with more than one). */
function ConsolePicker({
  open,
  onClose,
  profiles,
  activeId,
  onPick,
}: {
  open: boolean;
  onClose: () => void;
  profiles: PS5Profile[];
  activeId: string | null;
  onPick: (id: string) => void;
}) {
  const tr = useTr();
  const runtime = useConnectionStore((s) => s.runtimeByHost);
  return (
    <Modal open={open} onClose={onClose} title={tr("tv_switch_console", undefined, "Switch console")} size="lg">
      <ul className="space-y-3" data-testid="tv-console-list">
        {profiles.map((p) => {
          const rt = runtime[hostOf(p.host)];
          const color = lightOf(rt?.power ?? "unknown", rt?.payloadStatus === "up");
          const current = p.id === activeId;
          return (
            <li key={p.id}>
              <button
                type="button"
                onClick={() => onPick(p.id)}
                aria-current={current ? "true" : undefined}
                className={`flex min-h-14 w-full items-center gap-4 rounded-2xl border px-5 py-3 text-start text-lg font-semibold ${
                  current
                    ? "border-[var(--color-accent)] bg-[var(--color-accent-soft)]"
                    : "border-[var(--color-border)] bg-[var(--color-surface-3)] hover:bg-[var(--color-surface-raised)]"
                }`}
                data-testid={`tv-console-${p.id}`}
              >
                <span className="h-3 w-3 shrink-0 rounded-full" style={{ background: color }} aria-hidden />
                <span className="min-w-0 flex-1 break-words">{p.name || p.host}</span>
              </button>
            </li>
          );
        })}
      </ul>
    </Modal>
  );
}

/**
 * TV mode's home (#976): the console as one large card, with Play (or Resume) a single press
 * away. Play wakes a console in rest mode and opens the game in full screen; Play settings has
 * the few settings a TV needs; everything else is in the full app, one button away.
 */
export default function TvHome() {
  const tr = useTr();
  const navigate = useNavigate();
  const profiles = useRosterStore((s) => s.profiles);
  const activeId = useRosterStore((s) => s.active_id);
  const setActive = useRosterStore((s) => s.setActive);
  const setFullApp = useTvModeStore((s) => s.setFullApp);
  // The last console used is the selected one (the roster keeps it across starts).
  const profile = profiles.find((p) => p.id === activeId) ?? profiles[0] ?? null;
  const host = profile ? hostOf(profile.host) : "";
  const power = useConnectionStore((s) => (host ? (s.runtimeByHost[host]?.power ?? "unknown") : "unknown"));
  const payloadUp = useConnectionStore((s) => (host ? s.runtimeByHost[host]?.payloadStatus === "up" : false));
  const { stream, phase, target } = useRemotePlayStore(
    useShallow((s) => ({ stream: s.stream, phase: s.phase, target: s.target })),
  );
  const streaming = !!stream && phase !== null && phase !== "ended" && !!target;
  const paired = !!(profile?.wake_regist_key && profile?.wake_rp_key);
  const state = tvCardState({ hasConsole: !!profile, paired, streaming, power });
  const primary = tvPrimary(state);
  const { status: tailnet } = useTailnet(!!profile);
  const onTailnet = profile ? consoleOnTailnet(tailnet, profile.host) : null;
  const viaTailnet = !!onTailnet && onTailnet.via !== "lan";
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [pickerOpen, setPickerOpen] = useState(false);
  const { confirm, dialog } = useConfirm();

  // The remote starts on the main button (Play), and again when the card turns into another one
  // (paired, or a stream ended) while nothing else has the focus.
  const box = useRef<HTMLDivElement>(null);
  const firstFocus = useRef(true);
  useEffect(() => {
    const a = document.activeElement;
    const free = !a || a === document.body || !a.isConnected || !box.current?.contains(a);
    if (!firstFocus.current && !free) return;
    firstFocus.current = false;
    box.current?.querySelector<HTMLElement>(`[data-testid="${tvFocusTarget(primary)}"]`)?.focus({ preventScroll: true });
  }, [primary]);
  // Kept while away (layout/AppShell KeptScreens): back on show, the remote starts on it again.
  useEffect(
    () => () => {
      firstFocus.current = true;
    },
    [],
  );

  // Back on the TV home: asked first, then the app goes to the background (the Android app).
  useScreenBack(canLeaveApp(), () => {
    void leaveAfterAsking(
      () =>
        confirm({
          title: tr("tv_leave_title", undefined, "Leave PS5Upload?"),
          message: streaming
            ? tr("tv_leave_body_playing", undefined, "The game keeps streaming for a short while, so you can come back to it.")
            : undefined,
          confirmLabel: tr("tv_leave", undefined, "Leave"),
          cancelLabel: tr("tv_stay", undefined, "Stay"),
        }),
      () => leaveApp(),
    );
  });

  const streamingProfile = streaming ? (profiles.find((p) => hostOf(p.host) === hostOf(target!.host)) ?? null) : null;
  const playingName = target?.name || streamingProfile?.name || "";

  const play = () => {
    if (!profile) return;
    // Asked of the Play screen, not started here: its Start has the guards (a friend's game this
    // app joined is left first, a payload that blocks the picture is asked about).
    useStreamLaunch.getState().requestStream(profile.host);
    navigate("/play");
  };
  const resume = () => {
    // The stream shows on Play for the console selected: the one streaming, then.
    if (streamingProfile && streamingProfile.id !== profile?.id) setActive(streamingProfile.id);
    navigate("/play");
  };
  const stop = async () => {
    const ok = await confirm({
      title: tr("tv_stop_title", { name: playingName }, `Stop playing ${playingName}?`),
      confirmLabel: tr("rp_stop", undefined, "Stop"),
      cancelLabel: tr("tv_keep_playing", undefined, "Keep playing"),
      destructive: true,
    });
    if (ok) useRemotePlayStore.getState().stop();
  };
  const fullApp = () => {
    setFullApp(true);
    navigate("/home");
  };

  const name = profile ? profile.name || profile.host : tr("tv_no_console_title", undefined, "No PS5 yet");
  const color = state === "playing" ? "var(--color-good)" : lightOf(power, payloadUp);
  return (
    <div
      ref={box}
      className="mx-auto flex w-full max-w-5xl flex-col gap-6 px-4 py-6 sm:gap-8 sm:px-10 sm:py-10"
      data-testid="tv-home"
    >
      <header className="flex flex-wrap items-center gap-3">
        <AppLogo size={40} />
        <span className="text-2xl font-semibold tracking-[-0.02em]">PS5Upload</span>
        <div className="ms-auto">
          <TvButton icon={<LayoutGrid size={22} />} onClick={fullApp} testId="tv-full-app">
            {tr("tv_full_app", undefined, "Full app")}
          </TvButton>
        </div>
      </header>

      <section
        className="rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface-raised)] p-6 shadow-[var(--shadow-2)] sm:p-10"
        aria-labelledby="tv-console-name"
        data-testid="tv-console"
        data-state={state}
      >
        <div className="flex items-start gap-5">
          <span className="hidden h-20 w-20 shrink-0 place-items-center rounded-3xl bg-[var(--color-accent-soft)] text-[var(--color-accent-bright)] sm:grid">
            <MonitorPlay size={40} aria-hidden />
          </span>
          <div className="min-w-0 flex-1">
            <h1 id="tv-console-name" className="break-words text-3xl font-semibold sm:text-5xl" data-testid="tv-console-name">
              {name}
            </h1>
            <p className="mt-3 flex items-center gap-3 text-lg sm:text-2xl" data-testid="tv-console-state">
              {profile && <span className="h-3.5 w-3.5 shrink-0 rounded-full" style={{ background: color }} aria-hidden />}
              <span className="min-w-0">{stateLine(tr, state, playingName)}</span>
            </p>
            {viaTailnet && (
              <p className="mt-2 text-base text-[var(--color-muted)] sm:text-lg" data-testid="tv-console-tailnet">
                {tr("tv_via_tailscale", undefined, "Reached through Tailscale")}
              </p>
            )}
          </div>
        </div>

        <div className="mt-8 flex flex-wrap gap-4">
          {primary === "add" && (
            <TvButton primary icon={<Plus size={26} />} onClick={() => navigate("/connection?add=1")} testId="tv-add">
              {tr("tv_add_console", undefined, "Add your PS5")}
            </TvButton>
          )}
          {primary === "setup" && (
            <TvButton primary icon={<Wrench size={26} />} onClick={() => navigate("/play/setup")} testId="tv-setup">
              {tr("tv_setup", undefined, "Set up Remote Play")}
            </TvButton>
          )}
          {primary === "play" && (
            <TvButton primary icon={<PlayIcon size={26} />} onClick={play} testId="tv-play">
              {tr("play_title", undefined, "Play")}
            </TvButton>
          )}
          {primary === "resume" && (
            <TvButton primary icon={<PlayIcon size={26} />} onClick={resume} testId="tv-resume">
              {tr("tv_resume", undefined, "Resume")}
            </TvButton>
          )}
          {profile && (
            <TvButton icon={<Settings2 size={24} />} onClick={() => setSettingsOpen(true)} testId="tv-settings-open">
              {tr("tv_settings_title", undefined, "Play settings")}
            </TvButton>
          )}
          {primary === "resume" && (
            <TvButton icon={<Square size={22} />} onClick={() => void stop()} testId="tv-stop">
              {tr("rp_stop", undefined, "Stop")}
            </TvButton>
          )}
          {profiles.length > 1 && (
            <TvButton icon={<ArrowLeftRight size={24} />} onClick={() => setPickerOpen(true)} testId="tv-switch">
              {tr("tv_switch_console", undefined, "Switch console")}
            </TvButton>
          )}
        </div>
      </section>

      <section className="flex flex-wrap items-center gap-4" aria-label={tr("pt_title", undefined, "Play Together")}>
        <TvButton icon={<LogIn size={24} />} onClick={() => navigate("/play?mode=friend")} testId="tv-join-go">
          {tr("tv_join_button", undefined, "Join Play Together")}
        </TvButton>
        <p className="min-w-0 flex-1 text-base text-[var(--color-muted)] sm:text-lg">
          {tr("tv_join_body", undefined, "Got a link or a code from a friend? Play their PS5 on this TV.")}
        </p>
      </section>

      <TvPlaySettings
        open={settingsOpen}
        onClose={() => setSettingsOpen(false)}
        running={streaming}
        tailnet={tailnet}
        consoleTailnet={onTailnet}
      />
      <ConsolePicker
        open={pickerOpen}
        onClose={() => setPickerOpen(false)}
        profiles={profiles}
        activeId={profile?.id ?? null}
        onPick={(id) => {
          setPickerOpen(false);
          if (id !== profile?.id) setActive(id);
        }}
      />
      {dialog}
    </div>
  );
}
