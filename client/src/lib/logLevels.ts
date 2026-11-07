/*
 * Log levels (state/diagSettings): the engine owns the setting (`/api/logs/level`), keeps it on
 * disk and hands it to each PS5 helper. This module keeps the app's copy in step: it reads the
 * engine's on start, every half minute and when the window comes back, and changes it on the
 * engine. A raise above info ends by itself on the engine; the next read shows it.
 *
 * "Detailed logs" from before levels (a stored end time) is carried over once, as a debug raise
 * until that time.
 */

import {
  DEFAULT_LOG_LEVEL,
  KEY_DETAILED_UNTIL,
  LEVEL_RANK,
  MAX_RAISE_MS,
  isLevel,
  isRaise,
  useDiagSettingsStore,
  type EngineLevels,
  type LevelsChange,
} from "../state/diagSettings";
import { getEngineUrl } from "../state/engine";
import { log, type LogLevel } from "../state/logs";
import { safeGetItem, safeRemoveItem } from "./safeStorage";

/** How often the engine's setting is read again. */
export const REFRESH_MS = 30 * 1000;

/** The engine's answer as the store keeps it, or null when it is not one. */
export function parseLevels(v: unknown): EngineLevels | null {
  if (!v || typeof v !== "object") return null;
  const o = v as Record<string, unknown>;
  if (!isLevel(o.level)) return null;
  const cats: Partial<Record<string, EngineLevels["level"]>> = {};
  if (o.categories && typeof o.categories === "object")
    for (const [k, l] of Object.entries(o.categories as Record<string, unknown>)) if (isLevel(l)) cats[k] = l;
  const eff: Partial<Record<string, EngineLevels["level"]>> = {};
  if (o.effective && typeof o.effective === "object")
    for (const [k, l] of Object.entries(o.effective as Record<string, unknown>)) if (isLevel(l)) eff[k] = l;
  return {
    level: o.level,
    categories: cats,
    until_ms: typeof o.until_ms === "number" ? o.until_ms : null,
    raised: o.raised === true,
    effective: eff,
  };
}

/** Read the engine's setting into the store. False when the engine could not be reached (an
 *  engine from before levels answers 404: the store keeps the local fallback). */
export async function refreshLevels(fetchImpl: typeof fetch = fetch): Promise<boolean> {
  try {
    const r = await fetchImpl(`${getEngineUrl()}/api/logs/level`, { signal: AbortSignal.timeout(4000) });
    if (!r.ok) return false;
    const levels = parseLevels(await r.json());
    if (!levels) return false;
    useDiagSettingsStore.getState().setEngineLevels(levels);
    return true;
  } catch {
    return false;
  }
}

/** Change the engine's setting. Resolves to what is now in force; throws with the engine's words
 *  when it refused (an unknown level or category) or could not be reached. */
export async function changeLevels(change: LevelsChange, fetchImpl: typeof fetch = fetch): Promise<EngineLevels> {
  const r = await fetchImpl(`${getEngineUrl()}/api/logs/level`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(change),
    signal: AbortSignal.timeout(4000),
  });
  const body = (await r.json().catch(() => null)) as unknown;
  if (!r.ok) {
    const msg = (body as { error?: string } | null)?.error ?? `HTTP ${r.status}`;
    throw new Error(msg);
  }
  const levels = parseLevels(body);
  if (!levels) throw new Error("the engine's answer has no levels");
  useDiagSettingsStore.getState().setEngineLevels(levels);
  // The quiet part of the global level is also the local fallback and the shared setting.
  if (change.level || change.reset) useDiagSettingsStore.getState().setLogLevel(levels.level);
  const until = levels.until_ms ? ` until ${new Date(levels.until_ms).toLocaleString()}` : "";
  const cats = Object.entries(levels.categories)
    .map(([c, l]) => `${c}=${l}`)
    .join(", ");
  log.info("diag", `Log levels: ${levels.level}${cats ? `, ${cats}` : ""}${until}`);
  return levels;
}

/** The per-category overrides below info (a noisy category someone quietened). The reproduce
 *  button and its stop keep them: they are a choice, not part of a raise. */
export function loweredOverrides(cur: EngineLevels | null): Partial<Record<string, LogLevel>> {
  const out: Partial<Record<string, LogLevel>> = {};
  for (const [c, l] of Object.entries(cur?.categories ?? {})) if (l && LEVEL_RANK[l] > LEVEL_RANK.info) out[c] = l;
  return out;
}

/** What the reproduce button asks: everything at debug for `minutes`, the quietened categories
 *  as they are. */
export function reproduceChange(cur: EngineLevels | null, minutes: number): LevelsChange {
  return { level: "debug", categories: loweredOverrides(cur), minutes };
}

/** What "Stop recording more" asks: only what is above info comes down; a quieter global level
 *  and the quietened categories stay. */
export function stopRaiseChange(cur: EngineLevels | null): LevelsChange {
  const level = cur && !isRaise(cur.level) ? cur.level : DEFAULT_LOG_LEVEL;
  return { level, categories: loweredOverrides(cur) };
}

/** Everything at debug for `minutes`: the report screen's "while reproducing" button. */
export function raiseAllForReproducing(minutes = 30, fetchImpl: typeof fetch = fetch): Promise<EngineLevels> {
  return changeLevels(reproduceChange(useDiagSettingsStore.getState().engine, minutes), fetchImpl);
}

/** The reproduce button's stop: the raise ends now, everything else stays. */
export function stopRaising(fetchImpl: typeof fetch = fetch): Promise<EngineLevels> {
  return changeLevels(stopRaiseChange(useDiagSettingsStore.getState().engine), fetchImpl);
}

/** A "Detailed logs" switch left on by an older version: the same as a debug raise until then. */
async function carryOverDetailed(fetchImpl: typeof fetch): Promise<void> {
  const until = Number(safeGetItem(KEY_DETAILED_UNTIL));
  if (!safeGetItem(KEY_DETAILED_UNTIL)) return;
  const now = Date.now();
  try {
    if (Number.isFinite(until) && until > now && until <= now + MAX_RAISE_MS)
      await changeLevels({ level: "debug", until_ms: until }, fetchImpl);
    safeRemoveItem(KEY_DETAILED_UNTIL);
  } catch {
    // The engine is not up yet: tried again on the next start.
  }
}

let installed = false;

/** Keep the store in step with the engine. Idempotent. */
export function installLogLevels(fetchImpl: typeof fetch = fetch): () => void {
  if (installed || typeof window === "undefined") return () => {};
  installed = true;
  void refreshLevels(fetchImpl).then(() => carryOverDetailed(fetchImpl));
  const timer = setInterval(() => void refreshLevels(fetchImpl), REFRESH_MS);
  const onFocus = () => void refreshLevels(fetchImpl);
  window.addEventListener("focus", onFocus);
  return () => {
    clearInterval(timer);
    window.removeEventListener("focus", onFocus);
    installed = false;
  };
}
