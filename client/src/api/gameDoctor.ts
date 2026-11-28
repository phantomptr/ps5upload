// "Something wrong?" on the game page (engine GET /api/ps5/app/doctor): the usual reasons a game
// does not show on the PS5's home screen or does not start, checked in order. Read-only.
// A plain fetch against the engine origin, so the desktop app and the web UI take the same path.

import { getEngineUrl } from "../state/engine";
import { consoleAddr } from "../lib/addr";

export type DoctorStatus = "pass" | "problem" | "unknown";
export type DoctorCheckId =
  | "fake_package"
  | "mount_layer"
  | "package_size"
  | "database"
  | "permissions"
  | "free_space";
export type DoctorFix = "fix_permissions" | "restore" | "register" | "start_shadowmount";

export interface DoctorCheck {
  id: DoctorCheckId;
  status: DoctorStatus;
  /** A stable code the app words itself (`doctor_<id>_<reason>`). */
  reason: string;
  /** The engine's English, used when the app has no words for the reason. */
  message: string;
  params: Record<string, string | number>;
  fix: DoctorFix | null;
}

export interface DoctorAnswer {
  titleId: string;
  kind: "package" | "folder" | "image" | "unknown";
  /** The folder or image a registered game runs from. */
  source: string | null;
  firmware: string | null;
  checks: DoctorCheck[];
  firstProblem: DoctorCheckId | null;
}

const IDS: DoctorCheckId[] = [
  "fake_package",
  "mount_layer",
  "package_size",
  "database",
  "permissions",
  "free_space",
];
const FIXES: DoctorFix[] = ["fix_permissions", "restore", "register", "start_shadowmount"];
const KINDS: DoctorAnswer["kind"][] = ["package", "folder", "image", "unknown"];

const str = (v: unknown): string | null => (typeof v === "string" && v ? v : null);

/** The engine's answer in the app's shape; anything it doesn't know is dropped. */
export function normalizeDoctorAnswer(raw: unknown): DoctorAnswer {
  const b = (raw && typeof raw === "object" ? raw : {}) as Record<string, unknown>;
  const checks: DoctorCheck[] = [];
  for (const c of Array.isArray(b.checks) ? b.checks : []) {
    if (!c || typeof c !== "object") continue;
    const o = c as Record<string, unknown>;
    const id = o.id as DoctorCheckId;
    if (!IDS.includes(id)) continue;
    const status: DoctorStatus =
      o.status === "pass" || o.status === "problem" ? o.status : "unknown";
    const params: Record<string, string | number> = {};
    if (o.params && typeof o.params === "object") {
      for (const [k, v] of Object.entries(o.params as Record<string, unknown>)) {
        if (typeof v === "string" || typeof v === "number") params[k] = v;
      }
    }
    checks.push({
      id,
      status,
      reason: str(o.reason) ?? "",
      message: str(o.message) ?? "",
      params,
      fix: FIXES.includes(o.fix as DoctorFix) ? (o.fix as DoctorFix) : null,
    });
  }
  const kind = KINDS.includes(b.kind as DoctorAnswer["kind"]) ? (b.kind as DoctorAnswer["kind"]) : "unknown";
  const first = b.first_problem as DoctorCheckId;
  return {
    titleId: str(b.title_id) ?? "",
    kind,
    source: str(b.source),
    firmware: str(b.firmware),
    checks,
    firstProblem: IDS.includes(first) ? first : null,
  };
}

/** Runs every check for `titleId` on `host`. Many reads of the console: up to a minute. */
export async function gameDoctor(host: string, titleId: string): Promise<DoctorAnswer> {
  const q = `addr=${encodeURIComponent(consoleAddr(host))}&title_id=${encodeURIComponent(titleId)}`;
  const res = await fetch(`${getEngineUrl()}/api/ps5/app/doctor?${q}`, {
    signal: AbortSignal.timeout(120_000),
  });
  const body = (await res.json().catch(() => ({}))) as Record<string, unknown>;
  if (!res.ok) {
    throw new Error(typeof body.error === "string" && body.error ? body.error : `request failed (${res.status})`);
  }
  return normalizeDoctorAnswer(body);
}
