// Tasks started on the console page, PS5Upload Minion (#658, engine /api/ps5/console-tasks). They
// run on the console; any app connected to it reads them here and can stop them. A plain fetch
// against the engine origin, so the desktop app, the phone app and the web UI take the same path.

import { consoleAddr } from "../lib/addr";
import { getEngineUrl } from "../state/engine";

export interface ConsoleTaskInfo {
  /** A hex job id, or "smb:" and the SMB daemon's id: what cancel takes. */
  id: string;
  /** copy, move, delete, chmod, verify, unpack, extract, pkg_install, other. */
  kind: string;
  /** "gone": it ended and nobody saw how. */
  state: "running" | "done" | "failed" | "cancelled" | "gone";
  name: string;
  files_done: number;
  files_total: number;
  bytes_done: number;
  bytes_total: number;
  /** Seconds since it ended; 0 while it runs. */
  age_s: number;
  cancellable: boolean;
}

export interface ConsoleTaskList {
  /** False: a helper older than this feature, which has no such list. */
  supported: boolean;
  tasks: ConsoleTaskInfo[];
}

/** A refusal: `reason` is the helper's token (unknown_job, bad_job, unsupported, ...). */
export class ConsoleTaskError extends Error {
  readonly reason: string;
  constructor(message: string, reason: string) {
    super(message);
    this.name = "ConsoleTaskError";
    this.reason = reason;
  }
}

/** Reads a list reply, however odd: a missing list is an empty one. */
export function parseConsoleTaskList(body: unknown): ConsoleTaskList {
  const b = (body ?? {}) as { supported?: unknown; tasks?: unknown };
  return {
    supported: b.supported !== false,
    tasks: Array.isArray(b.tasks) ? (b.tasks as ConsoleTaskInfo[]).filter((t) => typeof t?.id === "string" && !!t.id) : [],
  };
}

async function reply(res: Response): Promise<Record<string, unknown>> {
  const body = (await res.json().catch(() => ({}))) as Record<string, unknown>;
  if (!res.ok) {
    throw new ConsoleTaskError(
      typeof body.error === "string" && body.error ? body.error : `request failed (${res.status})`,
      typeof body.reason === "string" ? body.reason : "failed",
    );
  }
  return body;
}

export async function fetchConsoleTasks(host: string): Promise<ConsoleTaskList> {
  const res = await fetch(
    `${getEngineUrl()}/api/ps5/console-tasks?addr=${encodeURIComponent(consoleAddr(host))}`,
    { signal: AbortSignal.timeout(12_000) },
  );
  return parseConsoleTaskList(await reply(res));
}

export async function cancelConsoleTask(host: string, id: string): Promise<void> {
  const res = await fetch(`${getEngineUrl()}/api/ps5/console-tasks/cancel`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ addr: consoleAddr(host), id }),
    signal: AbortSignal.timeout(20_000),
  });
  await reply(res);
}
