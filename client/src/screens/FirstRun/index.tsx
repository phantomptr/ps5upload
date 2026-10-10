import { useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router";
import {
  Sparkles,
  CheckCircle2,
  XCircle,
  CircleDashed,
  Send,
  ArrowRight,
  Cable,
  HardDrive,
  Box,
} from "lucide-react";
import {
  payloadsRelease,
  payloadsDownload,
  payloadsLocalPath,
  sendPayload,
  payloadCheck,
  portCheck,
  processList,
  type PayloadReleaseInfo,
} from "../../api/ps5";
import { mgmtAddr } from "../../lib/addr";
import { runningChainPayloads, type ChainPayload } from "../../lib/runningChain";
import { useConnectionStore, PS5_LOADER_PORT } from "../../state/connection";
import { helperSendFor, useHelperSendStore } from "../../state/helperSend";
import { sendHelperAndWait } from "../../state/helperSendRuntime";
import { PageHeader, Button, Spinner } from "../../components";
import { useTr } from "../../state/lang";
import { pushNotification } from "../../state/notifications";
import { selectConsoleByAddress, withConsolePrefix } from "../../state/roster";

/**
 * First-run setup wizard.
 *
 * Goal: take a brand-new user from "I just opened the app" to
 * "kstuff + ShadowMount+ + ps5upload all loaded and verified" in
 * one screen, four sections, no manual download juggling.
 *
 * Sections (each gates the next):
 *   1. Connect       — ip + reachability (uses existing portCheck)
 *   2. Install combo — download + send kstuff → SMP, sequenced with the
 *                      catalogue's autoload delays, then the helper through
 *                      sendHelperTo (the one send that joins a send under
 *                      way and opens pairing for an unpaired console)
 *   3. Done          — link to Upload
 *
 * Why no new Tauri commands: the wizard is pure orchestration over
 * primitives that already exist for the Payloads tab. Adding a
 * server-side "run-setup" command would couple this UX flow into
 * the Rust layer with no benefit; the renderer can sequence them
 * with much better progress UX and per-step error recovery.
 *
 * Re-runnable: nothing here is once-only. Connection offers the wizard
 * whenever the helper is not loaded (e.g. after a PS5 reboot), which is
 * when re-running it is useful.
 */

type StepState = "idle" | "busy" | "ok" | "fail";

const KSTUFF_CURRENT = "kstuff-echostretch";
const SMP_ID = "shadowmountplus";

/** A check asked for just before committing a new address; the commit remounts this screen
 *  (each console has its own screens), so the new mount runs it. */
let pendingFirstRunCheck = false;

export default function FirstRunScreen() {
  const tr = useTr();
  const navigate = useNavigate();
  const host = useConnectionStore((s) => s.host);
  // The field edits a draft: committing an address selects that console, which remounts this
  // screen, so it happens on blur / Enter / Check, never per keystroke.
  const [hostDraft, setHostDraft] = useState(host);
  const setStatus = useConnectionStore((s) => s.setStatus);
  // The shared helper send's current step (checking elfldr, sending, waiting), for its row.
  const helperNote = useHelperSendStore((s) => helperSendFor(s, host.trim())?.msg);

  const [step1, setStep1] = useState<StepState>("idle");
  const [step1Msg, setStep1Msg] = useState<string>(
    tr("first_run_step1_idle", undefined, "Enter your PS5's IP and check"),
  );
  const [step3, setStep3] = useState<StepState>("idle");
  const [step3Msg, setStep3Msg] = useState<string>("");
  const [step3Detail, setStep3Detail] = useState<InstallStep[]>([]);

  // Cancel flag — prevents the long install sequence from continuing
  // after the user navigates away or clicks Cancel. The setStep3 +
  // setStep3Detail effects below would otherwise fire on an unmounted
  // component (React 18 silently drops, but the latent in-flight
  // network calls still bill against the GitHub rate limit).
  const cancelled = useRef(false);
  useEffect(() => {
    return () => {
      cancelled.current = true;
    };
  }, []);

  /** Select the typed console, then check it (after the remount, if the address changed). */
  const commitAndCheck = (thenCheck: boolean) => {
    const value = hostDraft.trim();
    if (value === host.trim()) {
      if (thenCheck) void handleCheck();
      return;
    }
    if (thenCheck) pendingFirstRunCheck = true;
    selectConsoleByAddress(value);
  };
  useEffect(() => {
    if (!pendingFirstRunCheck) return;
    pendingFirstRunCheck = false;
    void handleCheck();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  async function handleCheck() {
    if (!host?.trim()) {
      setStep1("fail");
      setStep1Msg(
        tr(
          "first_run_step1_no_host",
          undefined,
          "Enter your PS5's IP address first.",
        ),
      );
      return;
    }
    setStep1("busy");
    setStep1Msg(
      tr(
        "first_run_step1_checking",
        { host, port: PS5_LOADER_PORT },
        `Checking ${host}:${PS5_LOADER_PORT}…`,
      ),
    );
    const ok = await portCheck(host, PS5_LOADER_PORT);
    if (!ok) {
      setStep1("fail");
      setStep1Msg(
        tr(
          "first_run_step1_unreachable",
          { host, port: PS5_LOADER_PORT },
          `Port ${PS5_LOADER_PORT} not open on ${host}. Is your PS5 jailbroken and on the same LAN?`,
        ),
      );
      return;
    }
    setStep1("ok");
    setStep1Msg(
      tr(
        "first_run_step1_ok",
        { host },
        `${host} is reachable on the loader port.`,
      ),
    );
    // Fire a payloadCheck to populate kernel/firmware in the store
    // so step 2's auto-pick has data to work with. Best-effort: if
    // no payload is loaded yet we have no kernel string and the
    // user picks manually.
    try {
      const status = await payloadCheck(host);
      if (status.reachable) {
        setStatus({
          payloadStatus: "up",
          payloadStatusHost: host,
          payloadVersion: status.payloadVersion,
          ps5Kernel: status.ps5Kernel,
          ucredElevated: status.ucredElevated,
          payloadProbing: false,
        });
      }
    } catch {
      // No payload yet → expected on first run, that's why we're
      // here. Auto-pick falls back to "current FW" default.
    }
  }

  // EchoStretch's kstuff resolves kernel symbols at runtime via the
  // SDK's NID table, so the same binary covers FW 1.00 → 12.x. No
  // FW-based variant pick is needed.

  /** Chain payloads already running on the console. Only knowable when our
   *  helper is up (the process list comes through it); on a cold console
   *  this is empty and everything is sent, as before. Best effort. */
  async function detectRunningChain(): Promise<Set<ChainPayload>> {
    try {
      const status = await payloadCheck(host);
      if (!status.reachable) return new Set();
      const { processes } = await processList(mgmtAddr(host));
      return runningChainPayloads(processes);
    } catch {
      return new Set();
    }
  }

  /** `helperOnly`: the console loads kstuff (and ShadowMount+) itself — an
   *  autoloader, etaHEN, elf-arsenal — so send only ps5upload. Sending kstuff
   *  on top stacks a second copy. */
  async function handleInstall(opts?: { helperOnly?: boolean }) {
    if (step1 !== "ok") return;
    const helperOnly = opts?.helperOnly === true;
    cancelled.current = false;
    setStep3("busy");
    setStep3Detail([]);
    const kstuffId = KSTUFF_CURRENT;

    const sequence: InstallStep[] = [
      { id: kstuffId, label: kstuffId, state: "idle" },
      { id: SMP_ID, label: tr("firstrun_shadowmount_label", "ShadowMount+"), state: "idle" },
      { id: "ps5upload", label: "ps5upload", state: "idle" },
    ];
    setStep3Detail([...sequence]);

    // Helper to mutate one row by id without losing the others.
    const updateStep = (id: string, patch: Partial<InstallStep>) => {
      setStep3Detail((prev) =>
        prev.map((s) => (s.id === id ? { ...s, ...patch } : s)),
      );
    };

    try {
      const running = helperOnly ? new Set<ChainPayload>() : await detectRunningChain();
      // ── kstuff + SMP: download from catalogue, then send ──────
      for (const id of [kstuffId, SMP_ID]) {
        if (cancelled.current) return;
        if (helperOnly) {
          updateStep(id, {
            state: "ok",
            note: tr("first_run_step_skipped_own", undefined, "skipped — your console loads it"),
          });
          continue;
        }
        if (running.has(id === kstuffId ? "kstuff" : "shadowmount")) {
          updateStep(id, {
            state: "ok",
            note: tr(
              "first_run_step_already_running",
              undefined,
              "already running — not sent again",
            ),
          });
          continue;
        }
        updateStep(id, {
          state: "busy",
          note: tr("first_run_note_fetching", undefined, "fetching latest release…"),
        });
        let release: PayloadReleaseInfo;
        try {
          release = await payloadsRelease(id, false);
        } catch (e) {
          updateStep(id, {
            state: "fail",
            note: tr(
              "first_run_note_release_failed",
              { error: String(e) },
              "release fetch: {error}",
            ),
          });
          throw e;
        }
        if (!release.picked_asset_url) {
          const note = tr(
            "first_run_note_no_asset",
            undefined,
            "no compatible asset in latest release",
          );
          updateStep(id, { state: "fail", note });
          throw new Error(`${id}: ${note}`);
        }

        // Skip the download if a same-version copy is already in the
        // cache — saves bandwidth on re-runs.
        const cachedPath = await payloadsLocalPath(id);
        let elfPath: string | null = cachedPath;
        if (!elfPath) {
          if (cancelled.current) return;
          updateStep(id, {
            state: "busy",
            note: tr(
              "first_run_note_downloading",
              { tag: release.tag, kb: (release.picked_asset_size / 1024).toFixed(0) },
              "downloading {tag} ({kb} KB)…",
            ),
          });
          const local = await payloadsDownload(
            id,
            release.picked_asset_url,
            release.tag,
          );
          elfPath = local.path;
        }
        if (!elfPath) {
          const note = tr(
            "first_run_note_no_elf",
            undefined,
            "no local ELF after download",
          );
          updateStep(id, { state: "fail", note });
          throw new Error(`${id}: ${note}`);
        }

        if (cancelled.current) return;
        updateStep(id, {
          state: "busy",
          note: tr(
            "first_run_note_sending",
            { host, port: PS5_LOADER_PORT },
            "sending to {host}:{port}…",
          ),
        });
        await sendPayload(host, elfPath);
        const sent = tr("first_run_note_sent", { tag: release.tag }, "sent {tag}");
        updateStep(id, { state: "ok", note: sent });

        // Wait the catalogue-recommended delay before the next
        // payload. kstuff needs ~3s to settle the kernel patches;
        // SMP ~1s. Skipping this races SMP into a half-patched
        // kernel and crashes both.
        const delay = id === kstuffId ? 3000 : 1000;
        if (delay > 0) {
          updateStep(id, {
            state: "busy",
            note: tr(
              "first_run_note_waiting",
              { n: delay / 1000 },
              "waiting {n}s before the next payload…",
            ),
          });
          await new Promise((r) => setTimeout(r, delay));
          updateStep(id, { state: "ok", note: sent });
        }
      }

      // ── The helper: the same send as Connection's Send helper ────
      // It records the helper up in the connection store, joins a send already running, and
      // opens pairing when the console answers but this app is not paired with it yet. While
      // it runs, the row shows the send's own step (see helperNote below).
      if (cancelled.current) return;
      updateStep("ps5upload", { state: "busy", note: undefined });
      const failure = await sendHelperAndWait(host, tr);
      if (failure !== null) {
        updateStep("ps5upload", { state: "fail", note: failure });
        throw new Error(failure);
      }
      const version = useConnectionStore.getState().payloadVersion;
      updateStep("ps5upload", {
        state: "ok",
        note: tr(
          "first_run_note_verified",
          { version: version ?? "?" },
          "verified — v{version}",
        ),
      });

      setStep3("ok");
      setStep3Msg(
        tr(
          "first_run_done",
          undefined,
          "All payloads loaded. You're ready to upload games and manage your PS5.",
        ),
      );
      pushNotification(
        "success",
        withConsolePrefix(
          host,
          tr("notif_first_run_done_title", undefined, "PS5 setup complete"),
        ),
        {
          body: tr(
            "notif_first_run_ready_body",
            { host },
            `ps5upload is running on ${host}.`,
          ),
          link: "/games",
        },
      );
    } catch (e) {
      setStep3("fail");
      setStep3Msg(e instanceof Error ? e.message : String(e));
      pushNotification(
        "error",
        withConsolePrefix(
          host,
          tr("notif_first_run_failed_title", undefined, "PS5 setup failed"),
        ),
        {
          body: e instanceof Error ? e.message : String(e),
          link: "/first-run",
        },
      );
    }
  }

  return (
    <div className="app-page">
      <PageHeader
        icon={Sparkles}
        title={tr("first_run_title", undefined, "Set up your PS5")}
        description={tr(
          "first_run_description_v3",
          undefined,
          "Loads what most set-ups need, in the right order: kstuff (lets fake packages install and run), ShadowMount+ (puts game images and folders on the PS5's home screen) and ps5upload's own helper. After the PS5 restarts, Connection offers this wizard again.",
        )}
      />
      <div className="mx-auto max-w-3xl space-y-4">
        <SetupCard
          index={1}
          icon={Cable}
          title={tr("first_run_step1_title", undefined, "Connect to your PS5")}
          state={step1}
          stateText={step1Msg}
        >
          <div className="flex items-center gap-2">
            <input
              value={hostDraft}
              onBlur={() => commitAndCheck(false)}
              onChange={(e) => {
                setHostDraft(e.target.value);
                setStep1("idle");
                setStep1Msg(
                  tr(
                    "first_run_step1_idle",
                    undefined,
                    "Enter your PS5's IP and check",
                  ),
                );
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") commitAndCheck(true);
              }}
              placeholder="192.168.1.50"
              inputMode="decimal"
              disabled={step3 === "busy"}
              className="flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-2 text-sm outline-none focus:border-[var(--color-accent)] disabled:opacity-50"
            />
            <Button
              variant="secondary"
              size="md"
              onClick={() => commitAndCheck(true)}
              disabled={!hostDraft.trim() || step1 === "busy" || step3 === "busy"}
              loading={step1 === "busy"}
            >
              {tr("first_run_check", undefined, "Check")}
            </Button>
          </div>
        </SetupCard>

        {step1 === "ok" && (
          <SetupCard
            index={2}
            icon={Box}
            title={tr(
              "first_run_step3_title",
              undefined,
              "Install the payload chain",
            )}
            state={step3}
            stateText={step3Msg}
          >
            <p className="mb-3 text-xs text-[var(--color-muted)]">
              {tr(
                "first_run_step3_hint",
                undefined,
                "Downloads kstuff + ShadowMount+ from GitHub on first run, then streams them to your PS5 in the right order with the catalogue's recommended delays. Cached locally, so re-running this is fast.",
              )}
            </p>
            <Button
              variant="primary"
              size="md"
              leftIcon={
                step3 === "busy" ? (
                  <Spinner size={14} tone="inherit" />
                ) : (
                  <Send size={14} />
                )
              }
              onClick={() => void handleInstall()}
              disabled={step3 === "busy"}
            >
              {step3 === "ok"
                ? tr("first_run_run_again", undefined, "Run again")
                : step3 === "busy"
                  ? tr("first_run_running", undefined, "Installing…")
                  : tr(
                      "first_run_run",
                      undefined,
                      "Download + send (kstuff → SMP → ps5upload)",
                    )}
            </Button>
            {step3 !== "busy" && (
              <Button
                variant="secondary"
                size="md"
                className="ml-2"
                onClick={() => void handleInstall({ helperOnly: true })}
              >
                {tr("first_run_helper_only", undefined, "Send only ps5upload")}
              </Button>
            )}
            <p className="mt-2 text-xs text-[var(--color-muted)]">
              {tr(
                "first_run_helper_only_hint",
                undefined,
                "Already load kstuff yourself (an autoloader, etaHEN, elf-arsenal)? Send only ps5upload — loading kstuff a second time stacks another copy. Payloads already running are skipped either way.",
              )}
            </p>
            {/* Cancel: the install is a multi-step chain with sleeps + several
                round trips. The cancel flag was only ever set on unmount, so
                a user with no button had no way to abort. Sets the flag the
                step loop already checks and resets the card to idle. */}
            {step3 === "busy" && (
              <Button
                variant="secondary"
                size="md"
                className="ml-2"
                onClick={() => {
                  cancelled.current = true;
                  setStep3("idle");
                  setStep3Msg("");
                }}
              >
                {tr("first_run_cancel", undefined, "Cancel")}
              </Button>
            )}
            {step3Detail.length > 0 && (
              <ul className="mt-3 space-y-1.5">
                {step3Detail.map((s) => (
                  <li key={s.id} className="flex items-center gap-2 text-xs">
                    <StepIcon state={s.state} />
                    <span className="font-medium">{s.label}</span>
                    {(() => {
                      const note =
                        s.id === "ps5upload" && s.state === "busy" ? (helperNote ?? s.note) : s.note;
                      return note ? (
                        <span className="text-[var(--color-muted)]">— {note}</span>
                      ) : null;
                    })()}
                  </li>
                ))}
              </ul>
            )}
          </SetupCard>
        )}

        {step3 === "ok" && (
          <SetupCard
            index={3}
            icon={HardDrive}
            title={tr("first_run_step4_title", undefined, "You're ready")}
            state="ok"
            stateText={tr("first_run_step4_done", undefined, "Setup complete")}
          >
            <p className="mb-3 text-xs text-[var(--color-muted)]">
              {tr(
                "first_run_step4_body_v2",
                undefined,
                "Drop a USB stick into your PS5 with .ffpkg game images and ShadowMount+ will auto-mount them — they'll appear in Games. To upload other files or install .pkg packages, use Upload and Install Package.",
              )}
            </p>
            <div className="flex flex-wrap gap-2">
              <Button
                variant="primary"
                size="md"
                rightIcon={<ArrowRight size={14} />}
                onClick={() => navigate("/games")}
              >
                {tr("first_run_go_library", undefined, "Open Games")}
              </Button>
              <Button
                variant="secondary"
                size="md"
                onClick={() => navigate("/upload")}
              >
                {tr("first_run_go_upload", undefined, "Open Upload")}
              </Button>
            </div>
          </SetupCard>
        )}
      </div>
    </div>
  );
}

interface InstallStep {
  id: string;
  label: string;
  state: StepState;
  note?: string;
}

function StepIcon({ state }: { state: StepState }) {
  if (state === "ok")
    return <CheckCircle2 size={14} className="text-[var(--color-good)]" />;
  if (state === "fail")
    return <XCircle size={14} className="text-[var(--color-bad)]" />;
  if (state === "busy")
    return (
      <Spinner size={14} tone="accent" />
    );
  return <CircleDashed size={14} className="text-[var(--color-muted)]" />;
}

function SetupCard({
  index,
  icon: Icon,
  title,
  state,
  stateText,
  children,
}: {
  index: number;
  icon: typeof Sparkles;
  title: string;
  state: StepState;
  stateText: string;
  children: React.ReactNode;
}) {
  const borderClass =
    state === "ok"
      ? "border-[var(--color-good)]"
      : state === "fail"
        ? "border-[var(--color-bad)]"
        : state === "busy"
          ? "border-[var(--color-accent)]"
          : "border-[var(--color-border)]";
  return (
    <section
      className={`rounded-lg border bg-[var(--color-surface-2)] p-5 transition-colors ${borderClass}`}
    >
      <header className="mb-4 flex items-center gap-3">
        <div className="flex h-7 w-7 shrink-0 items-center justify-center rounded-full bg-[var(--color-surface-3)] text-xs font-semibold tabular-nums">
          {index}
        </div>
        <Icon size={16} className="shrink-0" />
        <div className="min-w-0 flex-1">
          <div className="text-sm font-semibold">{title}</div>
          <div className="mt-0.5 flex items-center gap-1.5 text-xs text-[var(--color-muted)]">
            <StepIcon state={state} />
            <span className="truncate">{stateText}</span>
          </div>
        </div>
      </header>
      <div>{children}</div>
    </section>
  );
}
