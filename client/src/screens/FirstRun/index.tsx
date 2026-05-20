import { useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import {
  Sparkles,
  CheckCircle2,
  XCircle,
  CircleDashed,
  Loader2,
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
  bundledPayloadPath,
  sendPayload,
  payloadCheck,
  portCheck,
  type PayloadReleaseInfo,
} from "../../api/ps5";
import {
  useConnectionStore,
  PS5_LOADER_PORT,
} from "../../state/connection";
import { PageHeader, Button } from "../../components";
import { useTr } from "../../state/lang";
import { pushNotification } from "../../state/notifications";

/**
 * First-run setup wizard.
 *
 * Goal: take a brand-new user from "I just opened the app" to
 * "kstuff + ShadowMount+ + ps5upload all loaded and verified" in
 * one screen, four sections, no manual download juggling.
 *
 * Sections (each gates the next):
 *   1. Connect       — ip + reachability (uses existing portCheck)
 *   2. Install combo — download + send kstuff → SMP → ps5upload,
 *                      sequenced with the catalogue's autoload delays
 *                      (re-uses payloadsRelease + payloadsDownload +
 *                      sendPayload + payloadCheck primitives)
 *   3. Done          — link to Upload
 *
 * Why no new Tauri commands: the wizard is pure orchestration over
 * primitives that already exist for the Payloads tab. Adding a
 * server-side "run-setup" command would couple this UX flow into
 * the Rust layer with no benefit; the renderer can sequence them
 * with much better progress UX and per-step error recovery.
 *
 * Re-runnable: nothing here is once-only. Users can re-open the
 * wizard from Settings (or just navigate to /first-run) to refresh
 * a stuck setup, e.g. after a PS5 reboot.
 */

type StepState = "idle" | "busy" | "ok" | "fail";

const KSTUFF_CURRENT = "kstuff-echostretch";
const SMP_ID = "shadowmountplus";

/** When the bundled-ps5upload extraction populated cache, this is
 *  the conventional filename `probes.rs::find_bundled_payload` writes. */
function isBundledPs5UploadAvailable(path: string | null): boolean {
  return !!path && path.length > 0;
}

export default function FirstRunScreen() {
  const tr = useTr();
  const navigate = useNavigate();
  const host = useConnectionStore((s) => s.host);
  const setHost = useConnectionStore((s) => s.setHost);
  const setStatus = useConnectionStore((s) => s.setStatus);

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

  async function handleInstall() {
    if (step1 !== "ok") return;
    cancelled.current = false;
    setStep3("busy");
    setStep3Detail([]);
    const kstuffId = KSTUFF_CURRENT;

    const sequence: InstallStep[] = [
      { id: kstuffId, label: kstuffId, state: "idle" },
      { id: SMP_ID, label: "ShadowMount+", state: "idle" },
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
      // ── kstuff + SMP: download from catalogue, then send ──────
      for (const id of [kstuffId, SMP_ID]) {
        if (cancelled.current) return;
        updateStep(id, { state: "busy", note: "fetching latest release…" });
        let release: PayloadReleaseInfo;
        try {
          release = await payloadsRelease(id, false);
        } catch (e) {
          updateStep(id, { state: "fail", note: `release fetch: ${e}` });
          throw e;
        }
        if (!release.picked_asset_url) {
          updateStep(id, {
            state: "fail",
            note: "no compatible asset in latest release",
          });
          throw new Error(`no asset for ${id}`);
        }

        // Skip the download if a same-version copy is already in the
        // cache — saves bandwidth on re-runs.
        const cachedPath = await payloadsLocalPath(id);
        let elfPath: string | null = cachedPath;
        if (!elfPath) {
          if (cancelled.current) return;
          updateStep(id, {
            state: "busy",
            note: `downloading ${release.tag} (${(release.picked_asset_size / 1024).toFixed(0)} KB)…`,
          });
          const local = await payloadsDownload(
            id,
            release.picked_asset_url,
            release.tag,
          );
          elfPath = local.path;
        }
        if (!elfPath) {
          updateStep(id, { state: "fail", note: "no local ELF after download" });
          throw new Error(`no path for ${id}`);
        }

        if (cancelled.current) return;
        updateStep(id, { state: "busy", note: `sending to ${host}:${PS5_LOADER_PORT}…` });
        await sendPayload(host, elfPath);
        updateStep(id, { state: "ok", note: `sent ${release.tag}` });

        // Wait the catalogue-recommended delay before the next
        // payload. kstuff needs ~3s to settle the kernel patches;
        // SMP ~1s. Skipping this races SMP into a half-patched
        // kernel and crashes both.
        const delay = id === kstuffId ? 3000 : 1000;
        if (delay > 0) {
          updateStep(id, {
            state: "busy",
            note: `waiting ${delay / 1000}s before next payload…`,
          });
          await new Promise((r) => setTimeout(r, delay));
          updateStep(id, { state: "ok", note: `sent ${release.tag}` });
        }
      }

      // ── ps5upload: bundled, no GitHub fetch needed ────────────
      if (cancelled.current) return;
      updateStep("ps5upload", { state: "busy", note: "locating bundled ELF…" });
      const ourPath = await bundledPayloadPath();
      if (!isBundledPs5UploadAvailable(ourPath)) {
        updateStep("ps5upload", {
          state: "fail",
          note: "bundled ps5upload.elf not found",
        });
        throw new Error("ps5upload bundled missing");
      }
      updateStep("ps5upload", {
        state: "busy",
        note: `sending to ${host}:${PS5_LOADER_PORT}…`,
      });
      await sendPayload(host, ourPath);
      updateStep("ps5upload", { state: "ok", note: "sent" });

      // Probe verifies our payload booted and answers on the mgmt port.
      if (cancelled.current) return;
      updateStep("ps5upload", { state: "busy", note: "verifying boot…" });
      const status = await pollPayloadReady(host, 15);
      if (!status.reachable) {
        updateStep("ps5upload", {
          state: "fail",
          note: "ps5upload didn't answer within 15s",
        });
        throw new Error("ps5upload boot timeout");
      }
      setStatus({
        payloadStatus: "up",
        payloadStatusHost: host,
        payloadVersion: status.payloadVersion,
        ps5Kernel: status.ps5Kernel,
        ucredElevated: status.ucredElevated,
        payloadProbing: false,
      });
      updateStep("ps5upload", {
        state: "ok",
        note: `verified — v${status.payloadVersion ?? "?"}`,
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
        tr("notif_first_run_done_title", undefined, "PS5 setup complete"),
        {
          body: tr(
            "notif_first_run_done_body",
            { host },
            `Loaded kstuff + ShadowMount+ + ps5upload onto ${host}.`,
          ),
          link: "/library",
        },
      );
    } catch (e) {
      setStep3("fail");
      setStep3Msg(e instanceof Error ? e.message : String(e));
      pushNotification(
        "error",
        tr("notif_first_run_failed_title", undefined, "PS5 setup failed"),
        {
          body: e instanceof Error ? e.message : String(e),
          link: "/first-run",
        },
      );
    }
  }

  return (
    <div className="p-6">
      <PageHeader
        icon={Sparkles}
        title={tr("first_run_title", undefined, "Set up your PS5")}
        description={tr(
          "first_run_description",
          undefined,
          "One-click install of the recommended payload chain: kernel exploit (kstuff) + auto-mount daemon (ShadowMount+) + this app's payload. Re-run any time from Settings.",
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
              value={host}
              onChange={(e) => {
                setHost(e.target.value);
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
                if (e.key === "Enter") void handleCheck();
              }}
              placeholder="192.168.1.50"
              disabled={step3 === "busy"}
              className="flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-2 text-sm outline-none focus:border-[var(--color-accent)] disabled:opacity-50"
            />
            <Button
              variant="secondary"
              size="md"
              onClick={() => void handleCheck()}
              disabled={!host.trim() || step1 === "busy" || step3 === "busy"}
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
                  <Loader2 size={14} className="animate-spin" />
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
                  <li
                    key={s.id}
                    className="flex items-center gap-2 text-xs"
                  >
                    <StepIcon state={s.state} />
                    <span className="font-medium">{s.label}</span>
                    {s.note && (
                      <span className="text-[var(--color-muted)]">— {s.note}</span>
                    )}
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
            stateText={tr(
              "first_run_step4_done",
              undefined,
              "Setup complete",
            )}
          >
            <p className="mb-3 text-xs text-[var(--color-muted)]">
              {tr(
                "first_run_step4_body",
                undefined,
                "Drop a USB stick into your PS5 with .ffpkg game images and ShadowMount+ will auto-mount them — they'll appear in the Library tab. To upload arbitrary files or install .pkg packages, use the Upload and Install Package tabs.",
              )}
            </p>
            <div className="flex flex-wrap gap-2">
              <Button
                variant="primary"
                size="md"
                rightIcon={<ArrowRight size={14} />}
                onClick={() => navigate("/library")}
              >
                {tr("first_run_go_library", undefined, "Open Library")}
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
      <Loader2 size={14} className="animate-spin text-[var(--color-accent)]" />
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

/** Poll payloadCheck until reachable or timeout. Mirrors the
 *  Connection screen's pollUntilReady but local to this wizard so
 *  failures here don't block the dedicated screen's poller. */
async function pollPayloadReady(
  host: string,
  maxAttempts: number,
): Promise<{
  reachable: boolean;
  payloadVersion: string | null;
  ps5Kernel: string | null;
  ucredElevated: boolean | null;
}> {
  for (let i = 0; i < maxAttempts; i++) {
    await new Promise((r) => setTimeout(r, 1000));
    try {
      const r = await payloadCheck(host);
      if (r.reachable) return r;
    } catch {
      // ignore, keep polling
    }
  }
  return {
    reachable: false,
    payloadVersion: null,
    ps5Kernel: null,
    ucredElevated: null,
  };
}
