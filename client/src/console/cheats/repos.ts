import { compareVersions, sameVersion, versionParts } from "../../lib/versionNumbers";
/**
 * The community cheat repos, read by the PS5 browser itself (the console page has no PC engine to
 * ask). A port of the PC engine's engine/crates/ps5upload-core/src/cheats.rs (cheat_repos,
 * parse_cheat_filename, parse_index_line, format_from_index, tree_entry_from_path,
 * cheats_repo_search's de-duplication, cheats_repo_download's content URLs): keep the two in step.
 *
 * Both hosts answer CORS-open (Access-Control-Allow-Origin: *), and the helper's CSP allows exactly
 * these two in connect-src (payload/src/webui_api.c PAGE_HEADERS).
 */

export type CheatFormat = "json" | "shn" | "mc4";

export interface CheatRepo {
  id: string;
  name: string;
  /** Where the index files live (`json.txt`, ...). */
  rawBase: string;
  /** Index files in the repo root; empty when the repo is listed through `treeApi`. */
  indexFiles: string[];
  /** Where the `json/`, `shn/`, `mc4/` folders live (not always the root). */
  contentBase: string;
  /** GitHub git-trees API URL for a repo that publishes no index ("" = use indexFiles). */
  treeApi: string;
  /** The folder inside the tree holding the format folders (`cheats/`). */
  treePrefix: string;
}

/** cheats.rs cheat_repos(), in the same order (the first repo listing a file name wins). */
export const CHEAT_REPOS: readonly CheatRepo[] = [
  {
    id: "etahen",
    name: "etaHEN/PS5_Cheats",
    rawBase: "https://raw.githubusercontent.com/etaHEN/PS5_Cheats/main/",
    indexFiles: ["json.txt", "shn.txt", "mc4.txt"],
    contentBase: "https://raw.githubusercontent.com/etaHEN/PS5_Cheats/main/",
    treeApi: "",
    treePrefix: "",
  },
  {
    id: "goldhen",
    name: "GoldHEN/GoldHEN_Cheat_Repository",
    rawBase: "https://raw.githubusercontent.com/GoldHEN/GoldHEN_Cheat_Repository/main/",
    indexFiles: ["json.txt", "shn.txt", "mc4.txt"],
    contentBase: "https://raw.githubusercontent.com/GoldHEN/GoldHEN_Cheat_Repository/main/",
    treeApi: "",
    treePrefix: "",
  },
  {
    // No `*.txt` index, `master`, cheats nested under `cheats/`: listed through the git-trees API
    // (60 requests an hour without a login, so a failure is "this source is unavailable").
    id: "henmix",
    name: "TeeKay87/HEN-Cheats-Collection",
    rawBase: "https://raw.githubusercontent.com/TeeKay87/HEN-Cheats-Collection/master/",
    indexFiles: [],
    contentBase: "https://raw.githubusercontent.com/TeeKay87/HEN-Cheats-Collection/master/cheats/",
    treeApi: "https://api.github.com/repos/TeeKay87/HEN-Cheats-Collection/git/trees/master?recursive=1",
    treePrefix: "cheats/",
  },
];

export interface CheatEntry {
  filename: string;
  /** The game's name from the index ("" for a tree-listed repo). */
  gameTitle: string;
  format: CheatFormat;
  repoId: string;
  /** From the file name; "" when it breaks the convention. */
  titleId: string;
  /** The game version the cheat was made for; "" when the name does not say. */
  gameVersion: string;
}

/**
 * cheats.rs parse_cheat_filename: `<TITLE_ID>_<VERSION>[_<variant>].<ext>` → [title id, version].
 * A title id is four letters then five digits; the version is the next segment when it is digits
 * and dots with a dot (`_2` after it is a variant counter). Empty strings when it does not fit.
 */
export function parseCheatFilename(filename: string): [string, string] {
  const dot = filename.lastIndexOf(".");
  const stem = dot >= 0 ? filename.slice(0, dot) : filename;
  const parts = stem.split("_");
  const title = parts[0] ?? "";
  if (!/^[A-Za-z]{4}[0-9]{5}$/.test(title)) return ["", ""];
  const v = parts[1];
  const version = v !== undefined && v !== "" && /^[0-9.]+$/.test(v) && v.includes(".") ? v : "";
  return [title.toUpperCase(), version];
}

/** cheats.rs parse_index_line: `filename=game title`; blank and `#` lines skipped. */
export function parseIndexLine(line: string): [string, string] | null {
  const l = line.trim();
  if (!l || l.startsWith("#")) return null;
  const eq = l.indexOf("=");
  if (eq < 0) return null;
  const filename = l.slice(0, eq).trim();
  if (!filename) return null;
  return [filename, l.slice(eq + 1).trim()];
}

/** cheats.rs format_from_index. */
export function formatFromIndex(indexName: string): CheatFormat {
  if (indexName === "shn.txt") return "shn";
  if (indexName === "mc4.txt") return "mc4";
  return "json";
}

/**
 * cheats.rs tree_entry_from_path: exactly `<prefix><format>/<filename>`, the format a known folder and
 * the file's extension agreeing with it (any case); anything else (nested, README, unknown) null.
 */
export function treeEntryFromPath(path: string, prefix: string): [string, CheatFormat] | null {
  if (!path.startsWith(prefix)) return null;
  const rest = path.slice(prefix.length);
  const slash = rest.indexOf("/");
  if (slash < 0) return null;
  const dir = rest.slice(0, slash);
  const filename = rest.slice(slash + 1);
  if (!filename || filename.includes("/")) return null;
  if (dir !== "json" && dir !== "shn" && dir !== "mc4") return null;
  const dot = filename.lastIndexOf(".");
  if (dot < 0) return null;
  if (filename.slice(dot + 1).toLowerCase() !== dir) return null;
  return [filename, dir];
}

/**
 * cheats_repo_download's URLs, tried in this order until one answers: `<content base><dir>/<file>`
 * for json, shn, mc4, misc (the same order as the engine, whatever the entry's own format).
 */
export function contentUrls(repo: CheatRepo, filename: string): string[] {
  const base = repo.contentBase || repo.rawBase;
  return ["json", "shn", "mc4", "misc"].map((d) => `${base}${d}/${filename}`);
}

/** The format the file is stored as: by its extension (cheats_repo_download), json by default. */
export function storeFormat(filename: string): CheatFormat {
  const f = filename.toLowerCase();
  if (f.endsWith(".shn")) return "shn";
  if (f.endsWith(".mc4")) return "mc4";
  return "json";
}

/** The helper's rule for a stored name (webui_cheat_name_ok): only these can be downloaded here. */
export function storableName(filename: string): boolean {
  if (filename.length < 1 || filename.length > 128 || filename.startsWith(".") || filename.includes("..")) return false;
  if (!/^[A-Za-z0-9._-]+$/.test(filename)) return false;
  const fmt = storeFormat(filename);
  return filename.toLowerCase().endsWith(`.${fmt}`) && filename.length > fmt.length + 1;
}

// ---- versions ----

export { compareVersions, sameVersion, versionParts };

/** A game's cheat files, best first: made for `installed` first, then newest version first. */
export function rankEntries(entries: CheatEntry[], installed: string | null | undefined): CheatEntry[] {
  return [...entries].sort((a, b) => {
    const am = sameVersion(a.gameVersion, installed) ? 0 : 1;
    const bm = sameVersion(b.gameVersion, installed) ? 0 : 1;
    if (am !== bm) return am - bm;
    const v = compareVersions(b.gameVersion, a.gameVersion);
    if (v !== 0) return v;
    return a.filename < b.filename ? -1 : a.filename > b.filename ? 1 : 0;
  });
}

// ---- reading the repos ----

/** One source's files as (filename, game title, format). */
type Listing = [string, string, CheatFormat][];

export type FetchLike = (url: string, init?: RequestInit) => Promise<Response>;

/** A source this long without an answer counts as unreachable. */
export const REPO_TIMEOUT_MS = 20_000;

export class RepoError extends Error {
  /** true: nothing answered (no internet, DNS blocked, timed out); false: an HTTP error or bad data. */
  readonly network: boolean;
  constructor(network: boolean, message: string) {
    super(message);
    this.name = "RepoError";
    this.network = network;
  }
}

/** The longest index or tree reply read (the git tree is ~2 MB today). */
export const LISTING_MAX = 16 * 1024 * 1024;

/** A whole GET (headers and body) that must finish within `timeoutMs`. */
export type Fetched = { ok: true; body: ArrayBuffer } | { ok: false; status: number };

/**
 * GET `url` with one deadline for the reply AND its body (a body that stalls cannot hang the
 * caller), reading the body in chunks and giving up past `max` bytes (RepoError "too_large").
 * Nothing answering (or the deadline) is RepoError(network); an HTTP error is { ok: false }.
 */
export async function fetchLimited(f: FetchLike, url: string, timeoutMs: number, max: number): Promise<Fetched> {
  const stop = typeof AbortController === "function" ? new AbortController() : null;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const deadline = new Promise<never>((_, reject) => {
    timer = setTimeout(() => {
      stop?.abort();
      reject(new RepoError(true, `timed out ${url}`));
    }, timeoutMs);
  });
  // Every await below races the deadline: a fetch that ignores its signal still cannot hang us.
  const within = <T,>(p: Promise<T>): Promise<T> => Promise.race([p, deadline]);
  deadline.catch(() => {});
  let res: Response;
  try {
    try {
      res = await within(f(url, { method: "GET", cache: "no-store", credentials: "omit", signal: stop?.signal }));
    } catch (e) {
      throw e instanceof RepoError ? e : new RepoError(true, `unreachable ${url}`);
    }
    if (!res.ok) {
      void res.body?.cancel().catch(() => {});
      return { ok: false, status: res.status };
    }
    const tooLarge = () => {
      stop?.abort();
      return new RepoError(false, "too_large");
    };
    const len = Number(res.headers.get("Content-Length"));
    if (Number.isFinite(len) && len > max) {
      void res.body?.cancel().catch(() => {});
      throw tooLarge();
    }
    const reader = res.body && typeof res.body.getReader === "function" ? res.body.getReader() : null;
    if (!reader) {
      let buf: ArrayBuffer;
      try {
        buf = await within(res.arrayBuffer());
      } catch (e) {
        throw e instanceof RepoError ? e : new RepoError(true, `cut off ${url}`);
      }
      if (buf.byteLength > max) throw tooLarge();
      return { ok: true, body: buf };
    }
    const chunks: Uint8Array[] = [];
    let total = 0;
    try {
      for (;;) {
        const { done, value } = await within(reader.read());
        if (done) break;
        total += value.byteLength;
        if (total > max) {
          void reader.cancel().catch(() => {});
          throw tooLarge();
        }
        chunks.push(value);
      }
    } catch (e) {
      void reader.cancel().catch(() => {});
      throw e instanceof RepoError ? e : new RepoError(true, `cut off ${url}`);
    }
    const out = new Uint8Array(total);
    let at = 0;
    for (const c of chunks) {
      out.set(c, at);
      at += c.byteLength;
    }
    return { ok: true, body: out.buffer };
  } finally {
    clearTimeout(timer);
  }
}

async function fetchText(f: FetchLike, url: string, timeoutMs: number): Promise<string> {
  const r = await fetchLimited(f, url, timeoutMs, LISTING_MAX);
  if (!r.ok) throw new RepoError(false, `http_${r.status}`);
  return new TextDecoder().decode(r.body);
}

/** One repo's listing: its index files (any that answers counts), or its git tree. */
async function listRepo(repo: CheatRepo, f: FetchLike, timeoutMs: number): Promise<Listing> {
  if (repo.indexFiles.length === 0) {
    const text = await fetchText(f, repo.treeApi, timeoutMs);
    let parsed: unknown;
    try {
      parsed = JSON.parse(text);
    } catch {
      throw new RepoError(false, "bad_tree");
    }
    const nodes = (parsed as { tree?: unknown } | null)?.tree;
    if (!Array.isArray(nodes)) throw new RepoError(false, "bad_tree");
    const out: Listing = [];
    for (const node of nodes as { type?: unknown; path?: unknown }[]) {
      if (!node || node.type !== "blob" || typeof node.path !== "string") continue;
      const e = treeEntryFromPath(node.path, repo.treePrefix);
      if (e) out.push([e[0], "", e[1]]);
    }
    return out;
  }
  const results = await Promise.allSettled(
    repo.indexFiles.map(async (idx) => {
      const text = await fetchText(f, `${repo.rawBase}${idx}`, timeoutMs);
      const fmt = formatFromIndex(idx);
      const out: Listing = [];
      for (const line of text.split(/\r?\n/)) {
        const p = parseIndexLine(line);
        if (p) out.push([p[0], p[1], fmt]);
      }
      return out;
    }),
  );
  const ok = results.filter((r): r is PromiseFulfilledResult<Listing> => r.status === "fulfilled");
  if (ok.length === 0) throw (results[0] as PromiseRejectedResult).reason;
  return ok.flatMap((r) => r.value);
}

export interface RepoIndex {
  entries: CheatEntry[];
  /** Repos that could not be read (their files are missing from entries). */
  failed: string[];
  /** Every repo failed and none of them answered at all: the PS5 cannot reach GitHub. */
  offline: boolean;
}

/**
 * Every repo's files, de-duplicated by file name across repos (the first repo wins, as in
 * cheats_repo_search). Repos are read at the same time; one that fails is listed in `failed`.
 */
export async function readRepoIndex(f: FetchLike, repos: readonly CheatRepo[] = CHEAT_REPOS, timeoutMs = REPO_TIMEOUT_MS): Promise<RepoIndex> {
  const results = await Promise.allSettled(repos.map((r) => listRepo(r, f, timeoutMs)));
  const seen = new Set<string>();
  const entries: CheatEntry[] = [];
  const failed: string[] = [];
  let answered = false;
  results.forEach((r, i) => {
    const repo = repos[i];
    if (r.status === "rejected") {
      failed.push(repo.id);
      if (!(r.reason instanceof RepoError && r.reason.network)) answered = true;
      return;
    }
    answered = true;
    for (const [filename, gameTitle, format] of r.value) {
      if (seen.has(filename)) continue;
      seen.add(filename);
      const [titleId, gameVersion] = parseCheatFilename(filename);
      entries.push({ filename, gameTitle, format, repoId: repo.id, titleId, gameVersion });
    }
  });
  return { entries, failed, offline: !answered };
}

let cached: Promise<RepoIndex> | null = null;

/**
 * readRepoIndex once per page session (the git-trees API allows 60 reads an hour). A read where any
 * repo failed is not kept: the next Find asks again.
 */
export function repoIndex(f: FetchLike = (u, i) => fetch(u, i)): Promise<RepoIndex> {
  if (!cached) {
    const p = readRepoIndex(f);
    cached = p;
    p.then(
      (r) => {
        if (r.failed.length && cached === p) cached = null;
      },
      () => {
        if (cached === p) cached = null;
      },
    );
  }
  return cached;
}

/** Forget the session's index (tests). */
export function resetRepoIndex(): void {
  cached = null;
}

/** The entries for one game (by title id, any case). Names the helper cannot store are left out. */
export function entriesFor(index: RepoIndex, titleId: string): CheatEntry[] {
  const id = titleId.toUpperCase();
  return index.entries.filter((e) => e.titleId === id && storableName(e.filename));
}

/** The helper's limit (cheats.c CHEAT_FILE_MAX): a larger file is never loaded. */
export const CHEAT_FILE_MAX = 4 * 1024 * 1024;

/**
 * One cheat file's bytes from its repo: the content URLs in order (contentUrls), the first that
 * answers 2xx, read within the timeout and never past CHEAT_FILE_MAX. RepoError(network) when
 * nothing answered or the body stalled; "too_large"; "not_found" when every URL answered an error;
 * "bad_name" for a name the helper would refuse (never fetched).
 */
export async function downloadCheat(entry: CheatEntry, f: FetchLike = (u, i) => fetch(u, i), timeoutMs = REPO_TIMEOUT_MS): Promise<ArrayBuffer> {
  if (!storableName(entry.filename)) throw new RepoError(false, "bad_name");
  const repo = CHEAT_REPOS.find((r) => r.id === entry.repoId);
  if (!repo) throw new RepoError(false, "unknown_repo");
  let answered = false;
  for (const url of contentUrls(repo, entry.filename)) {
    let r: Fetched;
    try {
      r = await fetchLimited(f, url, timeoutMs, CHEAT_FILE_MAX);
    } catch (e) {
      // Nothing answered at this URL: try the next. A stalled or oversized body ends the download.
      if (e instanceof RepoError && e.network && /^unreachable /.test(e.message)) continue;
      throw e instanceof RepoError && e.network ? new RepoError(true, "cut_off") : e;
    }
    answered = true;
    if (!r.ok) continue;
    if (r.body.byteLength === 0) throw new RepoError(false, "empty");
    return r.body;
  }
  throw new RepoError(!answered, answered ? "not_found" : "unreachable");
}
