import { useEffect, useState } from "react";
import { useNavigate } from "react-router";
import { RotateCcw, ServerCrash } from "lucide-react";

import {
  engineDiagnose,
  engineProblem,
  engineRestart,
  type EngineDiagnosis,
  type EngineProblemKind,
} from "../lib/engineDiagnosis";
import { isTauriEnv } from "../lib/tauriEnv";
import { useConnectionStore } from "../state/connection";
import { setLiveEngineUrl } from "../state/engine";
import { useTr } from "../state/lang";
import { Button } from "./Button";

type Tr = ReturnType<typeof useTr>;

function explain(
  tr: Tr,
  kind: EngineProblemKind,
  d: EngineDiagnosis | null,
): { why: string; fix: string } {
  const url = d?.url ?? "";
  switch (kind) {
    case "missing_binary":
      return {
        why: tr(
          "engine_problem.missing.why",
          undefined,
          "The engine program is missing from the app's folder. Security software sometimes quarantines it.",
        ),
        fix: tr(
          "engine_problem.missing.fix",
          undefined,
          "Restore ps5upload-engine from your antivirus quarantine (or allow it), or reinstall the app.",
        ),
      };
    case "stopped":
      return {
        why: tr(
          "engine_problem.stopped.why",
          undefined,
          "The engine is not running: it stopped, or never started.",
        ),
        fix: tr(
          "engine_problem.stopped.fix",
          undefined,
          "Press Restart engine. If it stops again, the Logs screen shows why.",
        ),
      };
    case "proxy":
      return {
        why: tr(
          "engine_problem.proxy.why",
          undefined,
          "Requests to the engine went to a proxy set on this computer instead of to the engine.",
        ),
        fix: tr(
          "engine_problem.proxy.fix",
          undefined,
          "Turn off the proxy or VPN tool for local addresses (127.0.0.1), or start the app without the http_proxy / ALL_PROXY setting.",
        ),
      };
    case "port_blocked":
      return {
        why: tr(
          "engine_problem.port.why",
          { url },
          "Something holds the engine's address ({url}) but does not answer: another program on that port, or security software blocking connections to it.",
        ),
        fix: tr(
          "engine_problem.port.fix",
          undefined,
          "Press Restart engine. If that fails, allow ps5upload-engine in your firewall or antivirus, then restart the app.",
        ),
      };
    case "not_answering":
      return {
        why: tr(
          "engine_problem.slow.why",
          undefined,
          "The engine is running but does not answer.",
        ),
        fix: tr(
          "engine_problem.slow.fix",
          undefined,
          "Press Restart engine. Firewall or antivirus software that filters local connections can cause this too: allow ps5upload-engine.",
        ),
      };
    case "remote_down":
      return {
        why: tr(
          "engine_problem.remote.why",
          { url },
          "The engine this app is set to use ({url}) does not answer.",
        ),
        fix: tr(
          "engine_problem.remote.fix",
          undefined,
          "Check that the engine (Docker container or other computer) is running and reachable at that address, or change it in Settings.",
        ),
      };
    case "ok":
      return { why: "", fix: "" };
  }
}

/** Why the app cannot reach its engine, measured by the desktop shell, with the fix and a
 *  Restart button. In the web UI the engine is the server the page came from, so the panel
 *  only says the connection was lost. */
export function EngineProblem({ compact = false }: { compact?: boolean }) {
  const tr = useTr();
  const navigate = useNavigate();
  const setStatus = useConnectionStore((s) => s.setStatus);
  const desktop = isTauriEnv();
  const [diag, setDiag] = useState<EngineDiagnosis | null>(null);
  const [restarting, setRestarting] = useState(false);
  const [restartError, setRestartError] = useState<string | null>(null);

  useEffect(() => {
    if (!desktop) return;
    let cancelled = false;
    void engineDiagnose()
      .then((d) => {
        if (!cancelled) setDiag(d);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [desktop]);

  async function restart() {
    setRestarting(true);
    setRestartError(null);
    try {
      const url = await engineRestart();
      setLiveEngineUrl(url);
      setStatus({ engineStatus: "up", engineError: null });
    } catch (e) {
      setRestartError(e instanceof Error ? e.message : String(e));
      void engineDiagnose()
        .then(setDiag)
        .catch(() => {});
    } finally {
      setRestarting(false);
    }
  }

  const kind: EngineProblemKind = diag ? engineProblem(diag) : "not_answering";
  const text = desktop
    ? explain(tr, kind, diag)
    : {
        why: tr(
          "engine_problem.web.why",
          undefined,
          "This page lost its connection to the ps5upload server it came from.",
        ),
        fix: tr(
          "engine_problem.web.fix",
          undefined,
          "Check that the server (Docker container) is running, then reload this page.",
        ),
      };
  const canRestart = desktop && (diag?.local ?? true);

  return (
    <div
      className={compact ? "" : "mx-auto max-w-xl text-center"}
      data-testid="engine-problem"
    >
      {!compact && (
        <ServerCrash
          size={28}
          className="mx-auto mb-2 text-[var(--color-bad)]"
        />
      )}
      <p className="text-sm font-semibold">
        {tr(
          "engine_problem.title",
          undefined,
          "The app cannot reach its engine on this computer",
        )}
      </p>
      <p className="mt-1 text-xs text-[var(--color-muted)]">{text.why}</p>
      <p className="mt-1 text-xs text-[var(--color-text)]">{text.fix}</p>
      {diag?.probe_error && (
        <p className="mt-1 break-all font-mono text-[0.6875rem] text-[var(--color-muted)]">
          {diag.probe_error}
        </p>
      )}
      {restartError && (
        <p
          className="mt-1 break-all text-xs text-[var(--color-bad)]"
          role="alert"
        >
          {restartError}
        </p>
      )}
      <div
        className={`mt-2 flex flex-wrap gap-2 ${compact ? "" : "justify-center"}`}
      >
        {canRestart && (
          <Button
            size="sm"
            variant="primary"
            leftIcon={<RotateCcw size={13} />}
            loading={restarting}
            onClick={() => void restart()}
          >
            {tr("engine_problem.restart", undefined, "Restart engine")}
          </Button>
        )}
        <Button size="sm" variant="secondary" onClick={() => navigate("/logs")}>
          {tr("gate_view_logs", "View logs")}
        </Button>
      </div>
    </div>
  );
}
