import { useEffect, useState } from "react";
import { useNavigate } from "react-router";
import { ArrowUpCircle, CloudDownload } from "lucide-react";

import { useUpdateStore, type UpdatePhase } from "../state/update";
import { useToastStore } from "../state/toasts";
import { useTr, type Translator } from "../state/lang";
import { isTauriEnv } from "../lib/tauriEnv";
import { getAppVersion } from "../lib/appVersion";

/**
 * "Check for updates", where people look: the top bar on desktop and tablet, the foot of More on
 * a phone, and the command palette. One press checks GitHub now (the launch check runs once a
 * day) and says the answer: up to date, or the update bar appears. With an update already found
 * it lights up and opens Settings → Updates.
 *
 * The browser web UI has no button: the page updates with the engine (the Docker image) that
 * serves it.
 */

/** An update was found and is waiting to be downloaded or installed. */
export function updateWaiting(phase: UpdatePhase): string | null {
  return phase.kind === "available" ||
    phase.kind === "downloading" ||
    phase.kind === "downloaded" ||
    phase.kind === "download-failed"
    ? phase.result.latest_version
    : null;
}

/** Checks now and says the result; with an update already waiting, opens Settings → Updates. */
export async function checkForUpdatesNow(tr: Translator, openSettings: () => void): Promise<void> {
  const store = useUpdateStore.getState();
  if (updateWaiting(store.phase)) {
    openSettings();
    return;
  }
  await store.checkNow();
  const phase = useUpdateStore.getState().phase;
  const toast = useToastStore.getState().push;
  if (phase.kind === "up-to-date") {
    toast({
      tone: "success",
      message: tr(
        "update_up_to_date_toast",
        { version: phase.result.current_version },
        "You're on the latest version (v{version})",
      ),
    });
  } else if (phase.kind === "error") {
    toast({
      tone: "warn",
      message: tr(
        "update_check_failed_toast",
        { message: phase.message },
        "Couldn't check for updates: {message}",
      ),
    });
  }
  // An update found shows the update bar (UpdateToast) by itself.
}

function useAppVersion(): string {
  const [version, setVersion] = useState("");
  useEffect(() => {
    getAppVersion()
      .then(setVersion)
      .catch(() => setVersion(""));
  }, []);
  return version;
}

export function UpdateCheckButton({ variant }: { variant: "orb" | "chip" }) {
  const tr = useTr();
  const navigate = useNavigate();
  const phase = useUpdateStore((s) => s.phase);
  const version = useAppVersion();
  if (!isTauriEnv()) return null;

  const waiting = updateWaiting(phase);
  const checking = phase.kind === "checking";
  const label = waiting
    ? tr("update_available_button", { version: waiting }, "Update available: v{version}")
    : version
      ? tr("update_check_button_title", { version }, "Check for updates (you have v{version})")
      : tr("update_check_button", undefined, "Check for updates");
  const onClick = () => void checkForUpdatesNow(tr, () => navigate("/settings"));
  // Not the circular arrows: those read as "reload the page", which is the button next to it.
  const Icon = waiting ? ArrowUpCircle : CloudDownload;
  const icon = (
    <Icon
      size={variant === "orb" ? 18 : 15}
      aria-hidden
      className={`${checking ? "animate-spin" : ""} ${waiting ? "text-[var(--color-accent)]" : ""}`}
    />
  );

  if (variant === "orb") {
    return (
      <button
        type="button"
        onClick={onClick}
        disabled={checking}
        aria-label={label}
        title={label}
        className={`icon-orb-button relative ${
          waiting ? "ring-2 ring-[color-mix(in_srgb,var(--color-accent)_55%,transparent)]" : ""
        }`}
      >
        {icon}
        {waiting && (
          <span
            aria-hidden
            className="absolute end-1.5 top-1.5 h-2.5 w-2.5 rounded-full bg-[var(--color-accent)]"
          />
        )}
      </button>
    );
  }
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={checking}
      aria-label={label}
      title={label}
      className={`chip h-11 gap-2 px-3 text-xs tabular-nums ${
        waiting ? "font-medium text-[var(--color-accent)]" : "text-[var(--color-muted)]"
      }`}
    >
      {icon}
      {waiting
        ? tr("update_available_chip", { version: waiting }, "v{version} available")
        : version
          ? `v${version}`
          : tr("update_check_button", undefined, "Check for updates")}
    </button>
  );
}
