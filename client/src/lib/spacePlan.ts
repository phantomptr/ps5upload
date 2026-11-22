/**
 * Space planner: "I want to put a 95 GB game on this drive" → what to clear, move or uninstall,
 * ranked. Plain data, no React and no strings (the screen translates).
 *
 * Only three kinds of step are ever proposed, each through a mechanism the app already has:
 *  - leftover:  items the console Clean up scan offers (removed by cleanup apply);
 *  - move:      a Library folder or image copied to another console drive, then the original
 *               removed (the Games "Move");
 *  - uninstall: a game installed from a package whose copy is in the Collection on this
 *               computer, so it can be installed again.
 * A game with no copy anywhere is never a candidate, and nothing is ever moved to the computer.
 */

export type SpaceStepKind = "leftover" | "move" | "uninstall";

/** The groups in the order a plan uses them: leftovers are free to lose, a moved game stays
 *  playable, an uninstalled one has to be installed again. */
export const KIND_ORDER: SpaceStepKind[] = ["leftover", "move", "uninstall"];

export interface SpaceCandidate {
  /** Unique within one plan. */
  id: string;
  kind: SpaceStepKind;
  label: string;
  titleId?: string;
  /** What doing it frees on the target drive. */
  bytes: number;
  /** When the app last saw the title being played (ms), null when it never did or has no
   *  title to look up: shown as "unknown", ranked after games known to be a month old. */
  lastPlayedMs: number | null;
  /** uninstall: the Collection on this computer has a copy to install it again from. */
  hasCopy?: boolean;
  /** move: the drives (volume paths) it may go to. */
  moveTargets?: string[];
  /** leftover: the clean-up scan's item id. */
  cleanupId?: number;
  /** move: the Library path; uninstall: unused. */
  path?: string;
}

export interface SpacePlanInput {
  /** Bytes the new game needs on the target drive. */
  neededBytes: number;
  /** What already fits there (the console's own "likely fits" number). */
  roomBytes: number;
  candidates: SpaceCandidate[];
  /** Room on each other drive a move may use (volume path → bytes). */
  destRoom: Record<string, number>;
  nowMs: number;
}

export interface PlannedStep {
  candidate: SpaceCandidate;
  selected: boolean;
  /** move: where it goes, null when no drive has room for it alongside the other moves. */
  dest?: string | null;
}

export interface SpacePlan {
  /** Every usable candidate, grouped by KIND_ORDER and ranked within its group. */
  steps: PlannedStep[];
  /** How much has to be freed (0 when it already fits). */
  shortBytes: number;
  /** What the selected steps free. */
  freedBytes: number;
  enough: boolean;
  /** Bytes still missing with the selection (0 when enough). */
  missingBytes: number;
  /** Separate actions the selection takes (all leftovers count as one: one clean-up run). */
  actions: number;
}

const DAY_MS = 24 * 60 * 60 * 1000;

/** Coarse "last played" bands, oldest first. Within a band the larger item wins, so a plan
 *  takes one big game played two months ago over five small ones played that same month.
 *  Unknown comes after games known to be a month or more old: the app only sees what is played
 *  while it runs, so an unknown game may well be one played yesterday. */
export function ageBand(lastPlayedMs: number | null, nowMs: number): number {
  if (lastPlayedMs == null || lastPlayedMs <= 0) return 2.5;
  const days = (nowMs - lastPlayedMs) / DAY_MS;
  if (days >= 90) return 1;
  if (days >= 30) return 2;
  if (days >= 7) return 3;
  return 4;
}

/** Can this candidate be proposed at all. */
export function isUsable(c: SpaceCandidate): boolean {
  if (!(c.bytes > 0)) return false;
  if (c.kind === "uninstall") return c.hasCopy === true;
  if (c.kind === "move") return (c.moveTargets?.length ?? 0) > 0;
  return c.cleanupId !== undefined;
}

function rankCompare(nowMs: number) {
  return (a: SpaceCandidate, b: SpaceCandidate): number => {
    const ga = KIND_ORDER.indexOf(a.kind);
    const gb = KIND_ORDER.indexOf(b.kind);
    if (ga !== gb) return ga - gb;
    // Leftovers have nothing to play: largest first.
    if (a.kind !== "leftover") {
      const ba = ageBand(a.lastPlayedMs, nowMs);
      const bb = ageBand(b.lastPlayedMs, nowMs);
      if (ba !== bb) return ba - bb;
    }
    if (a.bytes !== b.bytes) return b.bytes - a.bytes;
    const la = a.lastPlayedMs ?? 0;
    const lb = b.lastPlayedMs ?? 0;
    if (la !== lb) return la - lb;
    return a.id < b.id ? -1 : a.id > b.id ? 1 : 0;
  };
}

/** Destinations for the moves in `ids`, in rank order: each goes to the drive with the most
 *  room left that can take it. A move no drive can take gets null. */
function assignMoves(ranked: SpaceCandidate[], ids: ReadonlySet<string>, destRoom: Record<string, number>): Map<string, string | null> {
  const room = new Map(Object.entries(destRoom));
  const out = new Map<string, string | null>();
  for (const c of ranked) {
    if (c.kind !== "move" || !ids.has(c.id)) continue;
    let best: string | null = null;
    for (const t of c.moveTargets ?? []) {
      const r = room.get(t) ?? 0;
      if (r >= c.bytes && (best === null || r > (room.get(best) ?? 0))) best = t;
    }
    if (best !== null) room.set(best, (room.get(best) ?? 0) - c.bytes);
    out.set(c.id, best);
  }
  return out;
}

function shortOf(input: Pick<SpacePlanInput, "neededBytes" | "roomBytes">): number {
  return Math.max(0, input.neededBytes - Math.max(0, input.roomBytes));
}

/** The plan for a given selection: destinations, totals, and what is still missing. A move
 *  that has nowhere to go frees nothing. */
export function planWithSelection(input: SpacePlanInput, selectedIds: ReadonlySet<string>): SpacePlan {
  const ranked = input.candidates.filter(isUsable).sort(rankCompare(input.nowMs));
  const dests = assignMoves(ranked, selectedIds, input.destRoom);
  let freed = 0;
  let actions = 0;
  let leftoverCounted = false;
  const steps: PlannedStep[] = ranked.map((c) => {
    const selected = selectedIds.has(c.id);
    const step: PlannedStep = { candidate: c, selected };
    if (c.kind === "move") step.dest = selected ? (dests.get(c.id) ?? null) : null;
    if (selected && (c.kind !== "move" || step.dest)) {
      freed += c.bytes;
      if (c.kind !== "leftover") actions++;
      else if (!leftoverCounted) {
        leftoverCounted = true;
        actions++;
      }
    }
    return step;
  });
  const shortBytes = shortOf(input);
  const missing = Math.max(0, shortBytes - freed);
  return { steps, shortBytes, freedBytes: freed, enough: missing === 0, missingBytes: missing, actions };
}

/** Picks from one group, in rank order, enough to cover `need`:
 *  - one item covering it alone wins (fewest actions), the best-ranked such item;
 *  - otherwise items are taken in rank order until covered, then any taken item the rest
 *    already cover without is put back (latest-ranked first).
 *  `fits` says whether an item can still be taken (moves need room on another drive). */
function pickFromGroup(
  group: SpaceCandidate[],
  need: number,
  fits: (c: SpaceCandidate, taken: SpaceCandidate[]) => boolean,
): SpaceCandidate[] {
  if (need <= 0) return [];
  const single = group.find((c) => c.bytes >= need && fits(c, []));
  if (single) return [single];
  const taken: SpaceCandidate[] = [];
  let sum = 0;
  for (const c of group) {
    if (sum >= need) break;
    if (!fits(c, taken)) continue;
    taken.push(c);
    sum += c.bytes;
  }
  if (sum < need) return taken;
  for (let i = taken.length - 1; i >= 0; i--) {
    if (sum - taken[i].bytes >= need) {
      sum -= taken[i].bytes;
      taken.splice(i, 1);
    }
  }
  return taken;
}

/** The proposed plan: leftovers first, then moves, then uninstalls, each group only for what
 *  the earlier ones left short. Every usable candidate is in `steps` so it can be ticked. */
export function planSpace(input: SpacePlanInput): SpacePlan {
  const ranked = input.candidates.filter(isUsable).sort(rankCompare(input.nowMs));
  const selected = new Set<string>();
  let need = shortOf(input);
  for (const kind of KIND_ORDER) {
    if (need <= 0) break;
    const group = ranked.filter((c) => c.kind === kind);
    let picked: SpaceCandidate[];
    if (kind === "leftover") {
      // All of it is one clean-up run, so taking more costs no extra action; still only as
      // much as is needed, largest first, so nothing is removed for no reason.
      picked = pickFromGroup(group, need, () => true);
    } else if (kind === "move") {
      picked = pickFromGroup(group, need, (c, taken) => {
        const ids = new Set([...taken.map((t) => t.id), c.id]);
        return assignMoves(ranked, ids, input.destRoom).get(c.id) != null;
      });
    } else {
      picked = pickFromGroup(group, need, () => true);
    }
    for (const c of picked) {
      selected.add(c.id);
      need -= c.bytes;
    }
  }
  return planWithSelection(input, selected);
}

/** Selected steps in the order "Do it" runs them: leftovers (one clean-up run), moves, then
 *  uninstalls. A move with nowhere to go is left out. */
export function runOrder(plan: SpacePlan): PlannedStep[] {
  return plan.steps.filter((s) => s.selected && (s.candidate.kind !== "move" || !!s.dest));
}

// ── Drives ───────────────────────────────────────────────────────────────────

/** The internal drive shows up as /user on some consoles and /data on others (one partition). */
export function isInternalPath(volume: string): boolean {
  return volume === "/data" || volume === "/user";
}

/** A volume path as the clean-up scan names drives: internal, ext0, usb0… */
export function driveToken(volume: string): string {
  if (isInternalPath(volume)) return "internal";
  const m = /^\/mnt\/([a-z]+\d+)$/.exec(volume);
  return m ? m[1] : volume;
}

export function sameDrive(a: string, b: string): boolean {
  return driveToken(a) === driveToken(b);
}

/** The drive (volume path) a console path lives on: longest matching volume wins, and the
 *  internal drive's /user and /data count as one. Null when no volume holds it. */
export function volumeOfPath(path: string, volumes: string[]): string | null {
  let best: string | null = null;
  for (const v of volumes) {
    const root = v.replace(/\/+$/, "");
    if (path === root || path.startsWith(`${root}/`)) {
      if (best === null || root.length > best.length) best = root;
    }
  }
  if (best) return best;
  // /user/app/... when only /data is listed (or the reverse): the same partition.
  if (path.startsWith("/user/") || path.startsWith("/data/")) return volumes.find(isInternalPath) ?? null;
  return null;
}

// ── Entry point link ─────────────────────────────────────────────────────────

/** What to offer once the plan has run: install a staged package again, or go back to the page
 *  the plan was opened from. */
export type SpacePlanRetry = { kind: "pkg"; path: string; label: string } | { kind: "back"; to: string; label: string };

export interface SpacePlanRequest {
  neededBytes: number;
  /** A volume path, or "internal". */
  drive: string;
  retry?: SpacePlanRetry;
}

/** The Clean up link that opens the planner with the size and drive filled in. */
export function spacePlanLink(req: SpacePlanRequest): string {
  const q = new URLSearchParams();
  q.set("plan", "1");
  q.set("need", String(Math.max(0, Math.round(req.neededBytes))));
  q.set("drive", req.drive);
  if (req.retry) {
    q.set("retry", req.retry.kind);
    q.set("retry_to", req.retry.kind === "pkg" ? req.retry.path : req.retry.to);
    q.set("retry_label", req.retry.label);
  }
  return `/cleanup?${q.toString()}`;
}

/** Reads `spacePlanLink`'s query back; null when the link does not ask for a plan. */
export function parseSpacePlanQuery(search: string | URLSearchParams): SpacePlanRequest | null {
  const q = typeof search === "string" ? new URLSearchParams(search) : search;
  if (q.get("plan") !== "1") return null;
  const need = Number(q.get("need") ?? "");
  const req: SpacePlanRequest = {
    neededBytes: Number.isFinite(need) && need > 0 ? need : 0,
    drive: q.get("drive") || "internal",
  };
  const kind = q.get("retry");
  const to = q.get("retry_to") ?? "";
  const label = q.get("retry_label") ?? "";
  if (kind === "pkg" && to) req.retry = { kind: "pkg", path: to, label };
  // Only an in-app page: a link can't send the planner anywhere else.
  else if (kind === "back" && to.startsWith("/") && !to.startsWith("//")) req.retry = { kind: "back", to, label };
  return req;
}

/** The drive a request names, as one of `volumes` ("internal" → /user or /data). */
export function resolveDrive(drive: string, volumes: string[]): string | null {
  if (drive === "internal" || isInternalPath(drive)) return volumes.find(isInternalPath) ?? null;
  return volumes.find((v) => v === drive) ?? null;
}

const GIB = 1024 ** 3;

/** GiB typed by a person (the unit sizes are shown in everywhere) → bytes. */
export function parseSizeGib(text: string): number | null {
  const n = Number(text.trim().replace(",", "."));
  if (!Number.isFinite(n) || n <= 0) return null;
  return Math.round(n * GIB);
}

/** Bytes → the GiB text the size box shows (one decimal, rounded up so it never asks for less). */
export function gibText(bytes: number): string {
  if (!(bytes > 0)) return "";
  return String(Math.ceil((bytes / GIB) * 10) / 10);
}

// ── Candidates from what the console and the Collection report ───────────────

/** The parts of a clean-up item the planner reads. */
export interface LeftoverSource {
  id: number;
  cat: string;
  drive: string;
  action: string;
  default: boolean;
  bytes: number;
  alloc_bytes: number;
  path: string;
  name?: string;
  title_id?: string;
  /** Kraken Mount uses it (#894): never offered. */
  kept?: string;
}

/** The parts of a Library entry the planner reads. */
export interface MoveSource {
  kind: "game" | "image";
  name: string;
  path: string;
  volume: string;
  size: number;
  imageFormat?: string | null;
  titleId?: string | null;
}

/** A game installed on the console, with its base package where the console keeps it. */
export interface InstalledSource {
  titleId: string;
  titleName: string;
  origin: "pkg" | "registered";
  system: boolean;
  /** The installed app.pkg (path and size), when it could be read. */
  base?: { path: string; size: number } | null;
}

export interface CandidateInput {
  /** Volume path of the drive to make room on. */
  target: string;
  /** Every writable console drive (volume paths). */
  volumes: string[];
  /** Mount points that are disk images (a path below one is inside an image, not on a drive). */
  imageMounts: string[];
  leftovers: LeftoverSource[];
  library: MoveSource[];
  /** Recursive sizes of Library game folders (a folder's own entry says 0). Missing = unknown. */
  folderBytes: Record<string, number>;
  /** Paths the console runs a game from (registered folders and images, mounted images), and
   *  ShadowMount+ image names. Moving one would break the game, so it is never offered. */
  inUse: { paths: Set<string>; imageNames: Set<string> };
  installed: InstalledSource[];
  /** Title ids (upper case) the Collection on this computer has a copy of. */
  collectionIds: Set<string>;
  /** Title ids (upper case) running now: never moved or uninstalled under the player. */
  running: Set<string>;
  lastPlayed: (titleId: string | null | undefined) => number | null;
  /** Whether Kraken Mount uses a path (it mounts it, or it sits in a folder Kraken Mount scans):
   *  such a package or game folder IS the game, so it is never moved or removed (#894). */
  krakenKeeps?: (path: string, isDir: boolean) => boolean;
}

/** Image kinds that hold a game (.ffpfs holds save data). */
const GAME_IMAGES = new Set(["exfat", "ffpkg", "ffpfsc"]);

/** A path's file name without its extension. */
export function leafName(path: string): string {
  const leaf = path.split("/").pop() ?? "";
  return leaf.replace(/\.[^.]+$/, "");
}

/** Everything that could free room on `target`, before ranking. */
export function buildCandidates(input: CandidateInput): SpaceCandidate[] {
  const token = driveToken(input.target);
  const out: SpaceCandidate[] = [];

  // 1. Leftovers: what Clean up would tick by itself, on this drive. Updates of installed games
  //    (A) stay in the Clean up list: removing one takes the game back a version.
  for (const l of input.leftovers) {
    if (l.drive !== token || l.cat === "A" || l.cat === "G" || l.action === "none" || !l.default) continue;
    if (l.kept || input.krakenKeeps?.(l.path, false)) continue;
    out.push({
      id: `leftover:${l.id}`,
      kind: "leftover",
      label: l.name || l.title_id || leafName(l.path) || l.path,
      titleId: l.title_id,
      bytes: l.alloc_bytes > 0 ? l.alloc_bytes : l.bytes,
      lastPlayedMs: null,
      cleanupId: l.id,
      path: l.path,
    });
  }

  // 2. Moves: Library games and game images on this drive that nothing runs from.
  const targets = input.volumes.filter((v) => !sameDrive(v, input.target) && !input.imageMounts.includes(v));
  const insideImage = (p: string) => input.imageMounts.some((m) => p === m || p.startsWith(`${m}/`));
  for (const e of input.library) {
    const vol = volumeOfPath(e.path, input.volumes) ?? e.volume;
    if (!sameDrive(vol, input.target) || insideImage(e.path)) continue;
    if (e.kind === "image" && !GAME_IMAGES.has(e.imageFormat ?? "")) continue;
    if (input.inUse.paths.has(e.path)) continue;
    if (input.krakenKeeps?.(e.path, e.kind === "game")) continue;
    if (e.kind === "image" && input.inUse.imageNames.has(leafName(e.path))) continue;
    if (e.titleId && input.running.has(e.titleId.toUpperCase())) continue;
    const bytes = e.kind === "image" ? e.size : (input.folderBytes[e.path] ?? 0);
    out.push({
      id: `move:${e.path}`,
      kind: "move",
      label: e.name,
      titleId: e.titleId ?? undefined,
      bytes,
      lastPlayedMs: input.lastPlayed(e.titleId),
      moveTargets: targets,
      path: e.path,
    });
  }

  // 3. Uninstalls: games installed from a package, on this drive, with a copy in the Collection.
  for (const t of input.installed) {
    if (t.origin !== "pkg" || t.system || !t.base) continue;
    const id = t.titleId.toUpperCase();
    if (input.running.has(id)) continue;
    const vol = volumeOfPath(t.base.path, input.volumes);
    if (!vol || !sameDrive(vol, input.target)) continue;
    out.push({
      id: `uninstall:${id}`,
      kind: "uninstall",
      label: t.titleName,
      titleId: t.titleId,
      // Only the base package: what else the PS5 removes with the game is not counted.
      bytes: t.base.size,
      lastPlayedMs: input.lastPlayed(t.titleId),
      hasCopy: input.collectionIds.has(id),
    });
  }
  return out;
}
