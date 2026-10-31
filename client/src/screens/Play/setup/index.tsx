import { useState, type ReactNode } from "react";
import { useNavigate } from "react-router";
import { Check, ChevronDown, ChevronRight, CircleDot, MonitorPlay, Play as PlayIcon } from "lucide-react";

import { remoteplayEnable } from "../../../api/ps5";
import { Button, Callout, PageHeader, Toggle } from "../../../components";
import { mgmtAddr, transferAddr } from "../../../lib/addr";
import { humanizePs5Error } from "../../../lib/humanizeError";
import { pairConsole } from "../../../lib/pairConsole";
import { useTr } from "../../../state/lang";
import { usePlaySetupMarks } from "../../../state/playSetup";
import { blockerNames } from "../../../state/rpBlockers";
import { payloadCardLink } from "../../Payloads/payloadTabs";
import { NpSignInSection } from "../../Accounts/NpSignInSection";
import { SETUP_ORDER, type SetupKey, type SetupState } from "./playSetup";
import { usePlaySetup } from "./usePlaySetup";

/**
 * Play › Set up: from a fresh console to a picture in the app, one step at a time. Each step
 * the app can read ticks itself (the console is asked again every few seconds); where the app
 * can do the step, its button is right there; the one only the console shows (rest mode) the
 * user ticks. The open step is always the next one to do.
 */
export default function PlaySetupScreen() {
  const tr = useTr();
  const navigate = useNavigate();
  const s = usePlaySetup();
  const homeHidden = usePlaySetupMarks((m) => m.homeHidden);
  const setHomeHidden = usePlaySetupMarks((m) => m.setHomeHidden);
  // The step shown open: the next one to do (null), one the user picked, or none.
  const [opened, setOpened] = useState<SetupKey | "none" | null>(null);
  // A step just ticked: follow on to the new next one.
  const [seenNext, setSeenNext] = useState(s.next);
  if (seenNext !== s.next) {
    setSeenNext(s.next);
    setOpened(null);
  }
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<{ step: SetupKey; text: string } | null>(null);
  const open = opened === null ? s.next : opened === "none" ? null : opened;
  const addr = s.host ? mgmtAddr(s.host) : "";

  const run = async (step: SetupKey, what: string, fn: () => Promise<unknown>) => {
    setBusy(what);
    setError(null);
    try {
      await fn();
      await s.refresh();
    } catch (e) {
      setError({ step, text: humanizePs5Error(e instanceof Error ? e.message : String(e)) });
    } finally {
      setBusy(null);
    }
  };

  const r = s.readiness;
  const serviceOff = !!r && r.service_enabled === 0;
  const steps: Record<SetupKey, { title: string; body: string; act?: ReactNode }> = {
    connect: {
      title: tr("play_setup_connect_title", undefined, "Connect to your PS5"),
      body: tr(
        "play_setup_connect_body",
        undefined,
        "Jailbreak the PS5, then let the app send its helper (Connection does it). Everything below is read from the console through it.",
      ),
      act: (
        <Button size="sm" variant="primary" onClick={() => navigate("/connection")}>
          {tr("rp_open_connection", undefined, "Open Connection")}
        </Button>
      ),
    },
    user: {
      title: tr("rpg_user_title", undefined, "Sign in to a user on the PS5"),
      body: tr(
        "rpg_user_body",
        undefined,
        "On the console, pick the user you will play as. Nobody has to stay on screen, but the user must be signed in. Use a user without a login passcode: a passcode stops the automatic sign-in when the console wakes.",
      ),
    },
    account: {
      title: tr("rpg_account_title", undefined, "Give that user an account ID"),
      body: tr(
        "play_setup_account_body",
        undefined,
        "Play pairs with an account ID. A user linked to PlayStation Network already has one; an offline user needs one: set it in Accounts (any ID works; write it down, saves made afterwards are tied to it).",
      ),
      act: (
        <Button size="sm" variant="primary" onClick={() => navigate("/accounts")}>
          {tr("remotePlay_show_accounts", undefined, "Show accounts")}
        </Button>
      ),
    },
    signin: {
      title: tr("rpg_signin_title", undefined, "Sign the account in to PlayStation Network (offline)"),
      body: tr(
        "play_setup_signin_body",
        undefined,
        "The PS5 only lets a signed-in account stream. Run the offline sign-in here (nothing is sent to Sony), then restart the PS5 and send the helper again: this step ticks by itself once the console is back.",
      ),
      act: s.connected && addr ? <NpSignInSection addr={addr} info={s.info} /> : undefined,
    },
    service: {
      title: tr("rpg_service_title", undefined, "Turn Remote Play on"),
      body: tr(
        "rpg_service_body",
        undefined,
        "Settings › System › Remote Play › Enable Remote Play. The app can switch it on for you, and on firmware 10.00 and later also allow it for this user. If Start then says Remote Play is switched off, turn it on in the PS5's own Settings: the console's switch is the one that counts.",
      ),
      act: r ? (
        <Button
          size="sm"
          variant="primary"
          disabled={busy !== null}
          onClick={() =>
            void run("service", "service", async () =>
              s.setReadiness(await remoteplayEnable(serviceOff ? "service" : "user", transferAddr(s.host))),
            )
          }
        >
          {serviceOff ? tr("remotePlay_turn_on", undefined, "Turn on") : tr("remotePlay_allow_user", undefined, "Allow")}
        </Button>
      ) : undefined,
    },
    rest: {
      title: tr("rpg_rest_title", undefined, "Let the PS5 be woken from rest mode"),
      body: tr(
        "rpg_rest_body",
        undefined,
        "Settings › System › Power Saving › Features Available in Rest Mode: turn on Stay Connected to the Internet and Enable Turning On PS5 from Network. Without them, a console in rest mode cannot be woken and its network sleeps after a few seconds. Only the console can show these.",
      ),
      act:
        s.states.rest === "ok" ? (
          <Button size="sm" variant="ghost" onClick={() => s.setRestDone(false)}>
            {tr("play_setup_rest_undo", undefined, "Not done yet")}
          </Button>
        ) : (
          <Button size="sm" variant="primary" onClick={() => s.setRestDone(true)} data-testid="play-setup-rest-done">
            {tr("play_setup_rest_done", undefined, "I've turned these on")}
          </Button>
        ),
    },
    pair: {
      title: tr("rpg_pair_title", undefined, "Pair this app with the console"),
      body: tr(
        "play_setup_pair_body",
        undefined,
        "One press registers this app with the PS5 (like pairing the PS Remote Play app) and keeps its keys on this device. Streaming, waking and signing in all use them; pair again after resetting the console or changing the user.",
      ),
      act: s.profileId ? (
        <Button
          size="sm"
          variant="primary"
          loading={busy === "pair"}
          disabled={busy !== null || !s.connected}
          onClick={() => void run("pair", "pair", () => pairConsole(s.host, s.profileId!))}
          data-testid="play-setup-pair"
        >
          {tr("play_setup_pair", undefined, "Pair")}
        </Button>
      ) : undefined,
    },
    blockers: {
      title: tr("play_setup_blockers_title", undefined, "No payload blocks Remote Play"),
      body: s.blockers?.length
        ? tr(
            "play_setup_blockers_body_on",
            { name: blockerNames(s.blockers) },
            `${blockerNames(s.blockers)} is on, and it blocks Remote Play's picture. Play offers to pause it each time you start, or turn it off on its card in Payloads. This step ticks by itself when it is off.`,
          )
        : tr(
            "play_setup_blockers_body",
            undefined,
            "Some payloads turn off the console's video encoder, which Remote Play needs: Hush does. This step ticks when none of them is on.",
          ),
      act: s.blockers?.length ? (
        <Button size="sm" variant="primary" onClick={() => navigate(payloadCardLink(s.blockers![0]))} data-testid="play-setup-blockers-open">
          {tr("rp_blocker_open_card", { name: blockerNames(s.blockers) }, `Open ${blockerNames(s.blockers)}`)}
        </Button>
      ) : undefined,
    },
    play: {
      title: tr("play_setup_play_title", undefined, "Play"),
      body: tr(
        "play_setup_play_body",
        undefined,
        "Open Play and press Start: the app wakes the console if it is resting, signs in, and streams. This step ticks with the first picture.",
      ),
      act: (
        <Button size="sm" variant="primary" leftIcon={<PlayIcon size={14} />} onClick={() => navigate("/play")}>
          {tr("play_setup_start", undefined, "Start playing")}
        </Button>
      ),
    },
  };

  const { done, total } = s.progress;
  return (
    <div className="app-page space-y-4">
      <PageHeader
        icon={MonitorPlay}
        title={tr("play_setup_title", undefined, "Set up Play")}
        description={tr(
          "play_setup_description",
          undefined,
          "Play streams your PS5 into this app over Remote Play. These steps get a console there once; each one ticks by itself when it's done.",
        )}
      />
      <div className="surface-panel space-y-4 p-5" data-testid="play-setup">
        <div className="space-y-1.5">
          <div className="flex items-center justify-between text-sm">
            <span className="font-medium">
              {s.next === null
                ? tr("play_setup_all_done", undefined, "Play is ready")
                : tr("play_setup_progress", { done, total }, `${done} of ${total} done`)}
            </span>
            {s.next === null && (
              <Button size="sm" variant="primary" leftIcon={<PlayIcon size={14} />} onClick={() => navigate("/play")}>
                {tr("play_title", undefined, "Play")}
              </Button>
            )}
          </div>
          <div className="h-1.5 overflow-hidden rounded-full bg-[var(--color-surface-3)]" aria-hidden>
            <div
              className="h-full rounded-full bg-[var(--color-good)] transition-[width] duration-500"
              style={{ width: `${(done / total) * 100}%` }}
            />
          </div>
        </div>
        <ol className="space-y-2">
          {SETUP_ORDER.map((key, i) => {
            const st = s.states[key];
            const isOpen = open === key;
            const step = steps[key];
            return (
              <li
                key={key}
                className={`rounded-xl border p-3 transition-colors ${
                  isOpen ? "border-[var(--color-accent)]" : "border-[var(--color-border)]"
                }`}
                data-step={key}
                data-state={st}
              >
                <button
                  type="button"
                  className="flex w-full items-center gap-3 text-start"
                  aria-expanded={isOpen}
                  onClick={() => setOpened(isOpen ? "none" : key)}
                >
                  <StepMark state={st} n={i + 1} />
                  <span className={`min-w-0 flex-1 text-sm ${st === "todo" || st === "manual" ? "font-medium" : ""}`}>
                    {step.title}
                  </span>
                  <StateText state={st} tr={tr} />
                  {isOpen ? <ChevronDown size={16} aria-hidden /> : <ChevronRight size={16} aria-hidden />}
                </button>
                {isOpen && (
                  <div className="mt-2 space-y-3 ps-9 text-sm text-[var(--color-muted)]">
                    <p>{step.body}</p>
                    {st !== "wait" && step.act}
                    {st === "wait" && (
                      <p className="text-xs">
                        {tr("play_setup_wait", undefined, "Do the steps above first; this one opens when they're done.")}
                      </p>
                    )}
                    {error?.step === key && <Callout tone="error" title={error.text} />}
                  </div>
                )}
              </li>
            );
          })}
        </ol>
        <p className="text-xs text-[var(--color-muted)]">
          {tr(
            "play_setup_other_apps",
            undefined,
            "Want Sony's PS Remote Play app or chiaki-ng instead? Accounts › Remote Play makes the PIN they ask for.",
          )}
        </p>
        <Toggle
          checked={!homeHidden}
          onChange={(on) => setHomeHidden(!on)}
          label={tr("play_home_show", undefined, "Show Play on Home")}
        />
      </div>
    </div>
  );
}

function StepMark({ state, n }: { state: SetupState; n: number }) {
  const tone =
    state === "ok"
      ? "bg-[color-mix(in_srgb,var(--color-good)_18%,transparent)] text-[var(--color-good)]"
      : state === "todo" || state === "manual"
        ? "bg-[color-mix(in_srgb,var(--color-accent)_18%,transparent)] text-[var(--color-accent)]"
        : "bg-[var(--color-surface-3)] text-[var(--color-muted)]";
  return (
    <span className={`grid h-6 w-6 shrink-0 place-items-center rounded-full text-xs font-semibold ${tone}`} aria-hidden>
      {state === "ok" ? <Check size={13} /> : n}
    </span>
  );
}

function StateText({ state, tr }: { state: SetupState; tr: ReturnType<typeof useTr> }) {
  const text =
    state === "ok"
      ? tr("play_setup_state_done", undefined, "Done")
      : state === "todo"
        ? tr("play_setup_state_todo", undefined, "To do")
        : state === "manual"
          ? tr("rpg_on_console", undefined, "check on the console")
          : tr("play_setup_state_wait", undefined, "Waiting");
  return (
    <span className="flex shrink-0 items-center gap-1 text-xs text-[var(--color-muted)]">
      {state === "manual" && <CircleDot size={10} aria-hidden />}
      {text}
    </span>
  );
}
