import type { BackportRecord } from "../lib/backport";
import { hostOf } from "../lib/addr";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";

/* v2: records gained `setId` (replacing `donorTitleId`), plus
 * `replaced`/`stashDir` so an undo can restore libraries the install
 * displaced. A v1 record cannot describe that, and honouring one would offer
 * an undo that silently loses the title's original libraries — so the key is
 * bumped rather than migrated. */
const KEY = "ps5upload.backports.v3";
type Records = Record<string, BackportRecord>;

const recordKey = (host: string, titleId: string) => `${hostOf(host)}:${titleId}`;

function validRecord(value: unknown): value is BackportRecord {
  if (!value || typeof value !== "object") return false;
  const r = value as Partial<BackportRecord>;
  return typeof r.titleId === "string" &&
    typeof r.setId === "string" &&
    typeof r.targetSource === "string" &&
    typeof r.stashDir === "string" &&
    typeof r.complete === "boolean" &&
    Array.isArray(r.replaced) &&
    r.replaced.every((lib) => !!lib && typeof lib === "object" &&
      typeof lib.name === "string" && /^[^/\\]+\.(?:sprx|prx)$/i.test(lib.name) &&
      typeof lib.size === "number" && Number.isSafeInteger(lib.size) && lib.size >= 0) &&
    Array.isArray(r.copiedPaths) &&
    r.copiedPaths.every((p) => typeof p === "string" && p.startsWith(`${r.targetSource}/fakelib/`));
}

function loadAll(): Records {
  try {
    const raw = safeGetItem(KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    return Object.fromEntries(
      Object.entries(parsed).filter(([, value]) => validRecord(value)),
    );
  } catch {
    return {};
  }
}

export function loadBackportRecord(host: string, titleId: string): BackportRecord | null {
  return loadAll()[recordKey(host, titleId)] ?? null;
}

export function saveBackportRecord(host: string, record: BackportRecord): void {
  const records = loadAll();
  records[recordKey(host, record.titleId)] = record;
  safeSetItem(KEY, JSON.stringify(records));
}

export function removeBackportRecord(host: string, titleId: string): void {
  const records = loadAll();
  delete records[recordKey(host, titleId)];
  safeSetItem(KEY, JSON.stringify(records));
}
