import { invoke } from "./invokeLogged";

/** What the desktop shell measured about an engine the app cannot reach (engine_diagnose). */
export interface EngineDiagnosis {
  url: string;
  /** The app's own engine on this machine, not a remote one. */
  local: boolean;
  answering: boolean;
  probe_error: string | null;
  /** The shell still holds a live engine process (local only). */
  child_running: boolean | null;
  /** Something listens on the engine's port (local only). */
  port_taken: boolean | null;
  /** The bundled engine program is on disk (local only). */
  binary_found: boolean | null;
  binary_error: string | null;
  log_path: string | null;
  os: string;
}

export function engineDiagnose(): Promise<EngineDiagnosis> {
  return invoke<EngineDiagnosis>("engine_diagnose");
}

/** Stops and starts the app's engine. Resolves with where it now listens. */
export function engineRestart(): Promise<string> {
  return invoke<string>("engine_restart");
}

/** The cause, in words, of an unreachable engine. Picked from what was measured, never
 *  guessed: each kind names the fact that chose it. */
export type EngineProblemKind =
  /** The engine answers: nothing to explain. */
  | "ok"
  /** The engine program is not where the app installed it (security software removes or
   *  quarantines it, or the install is incomplete). */
  | "missing_binary"
  /** No engine process, and nothing on its port: it stopped or never started. */
  | "stopped"
  /** Requests went to a proxy set in the environment instead of this computer. */
  | "proxy"
  /** Something holds the engine's port but does not answer (security software filtering
   *  loopback, or another program on the port). */
  | "port_blocked"
  /** The engine runs but does not answer in time (busy or stuck). */
  | "not_answering"
  /** A remote engine (Docker, another computer) does not answer. */
  | "remote_down";

export function engineProblem(d: EngineDiagnosis): EngineProblemKind {
  if (d.answering) return "ok";
  if (!d.local) return "remote_down";
  const err = (d.probe_error ?? "").toLowerCase();
  if (/proxy|socks/.test(err)) return "proxy";
  if (d.binary_found === false) return "missing_binary";
  if (d.port_taken === false) return "stopped";
  if (d.child_running === false) return "port_blocked";
  return "not_answering";
}

export { isEngineUnreachable } from "./engineUnreachable";
