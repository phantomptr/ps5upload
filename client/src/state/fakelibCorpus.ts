import { resolveSets, type FakelibSet } from "../lib/backport";
import { getEngineUrl } from "./engine";

/* The backport library corpus.
 *
 * These are Sony system libraries, so we cannot ship them: every corpus is
 * built by the user from games they own, either by importing a pack or by
 * scanning a console and harvesting the fakelib/ of games that are already
 * backported. The engine owns the storage — there is no path to configure and
 * none to get wrong — so everything here is a thin call across.
 *
 * Nothing throws. A missing corpus is the ordinary state for a new user, and
 * the UI has to offer the two ways to build one rather than report a failure
 * the user cannot act on. */

export interface CorpusSummary {
  sets: number;
  builds: number;
  bytes: number;
}

export interface FakelibCorpus {
  sets: FakelibSet[];
  summary: CorpusSummary;
  /** Where the engine keeps it. Shown in Settings so "Reveal" means something. */
  root: string;
  error: string | null;
}

export const EMPTY_CORPUS: FakelibCorpus = {
  sets: [],
  summary: { sets: 0, builds: 0, bytes: 0 },
  root: "",
  error: null,
};

async function errorText(response: Response): Promise<string> {
  const body: unknown = await response.json().catch(() => null);
  return body && typeof body === "object" && "error" in body
    ? String((body as { error: unknown }).error)
    : `HTTP ${response.status}`;
}

export async function loadFakelibCorpus(): Promise<FakelibCorpus> {
  try {
    const response = await fetch(`${getEngineUrl()}/api/fakelibs/corpus`);
    if (!response.ok) {
      return { ...EMPTY_CORPUS, error: await errorText(response) };
    }
    const body = (await response.json()) as {
      manifest?: unknown;
      summary?: Partial<CorpusSummary>;
      root?: string;
    };
    return {
      sets: resolveSets(body.manifest),
      summary: {
        sets: body.summary?.sets ?? 0,
        builds: body.summary?.builds ?? 0,
        bytes: body.summary?.bytes ?? 0,
      },
      root: body.root ?? "",
      error: null,
    };
  } catch (e) {
    return { ...EMPTY_CORPUS, error: e instanceof Error ? e.message : String(e) };
  }
}

export interface ImportOutcome {
  setId: string | null;
  /** True when the corpus already had exactly these libraries. Not a failure —
   *  it is what makes re-importing the same pack safe. */
  duplicate: boolean;
  /** Files dropped because they are not libraries, so the UI can say which. A
   *  Mac leaves a `._x.sprx` AppleDouble beside every real file. */
  ignored: string[];
}

/** Send one pack as ONE set. Sets are installed whole and never merged, so what
 *  the user selects here is the unit they will later install. */
export async function importFakelibSet(
  label: string,
  source: string,
  files: File[],
): Promise<ImportOutcome> {
  const form = new FormData();
  files.forEach((file, i) => form.append(`f${i}`, file, file.name));
  const query = `label=${encodeURIComponent(label)}&source=${encodeURIComponent(source)}`;
  const response = await fetch(`${getEngineUrl()}/api/fakelibs/import?${query}`, {
    method: "POST",
    body: form,
  });
  if (!response.ok) throw new Error(await errorText(response));
  const body = (await response.json()) as {
    set_id?: string | null;
    duplicate?: boolean;
    ignored?: string[];
  };
  return {
    setId: body.set_id ?? null,
    duplicate: !!body.duplicate,
    ignored: body.ignored ?? [],
  };
}

export async function deleteFakelibSet(id: string): Promise<void> {
  const response = await fetch(`${getEngineUrl()}/api/fakelibs/set/${encodeURIComponent(id)}`, {
    method: "DELETE",
  });
  if (!response.ok) throw new Error(await errorText(response));
}

export interface ScanTitleInput {
  title_id: string;
  title_name: string;
  /** Disk-image (ShadowMount) title. Sent so the sighting records how the
   *  source game is stored, without a second lookup per title. */
  image_backed: boolean;
  source: string;
}

export interface ScanProgress {
  done: boolean;
  titlesTotal: number;
  titlesDone: number;
  current: string;
  /** `[label, libraryCount]` for each set added. */
  added: [string, number][];
  /** Titles whose libraries the corpus already had. */
  skipped: number;
  /** Titles with no fakelib/ — not backported, nothing to harvest. */
  withoutLibraries: number;
  errors: string[];
}

export async function startFakelibScan(
  addr: string,
  consoleName: string,
  titles: ScanTitleInput[],
): Promise<string> {
  const response = await fetch(`${getEngineUrl()}/api/fakelibs/scan`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ addr, console: consoleName, titles }),
  });
  if (!response.ok) throw new Error(await errorText(response));
  const body = (await response.json()) as { scan_id: string };
  return body.scan_id;
}

/** Poll one scan. A finished scan is forgotten by the engine after it is read,
 *  so callers must keep the last snapshot rather than re-reading it. */
export async function pollFakelibScan(id: string): Promise<ScanProgress | null> {
  const response = await fetch(`${getEngineUrl()}/api/fakelibs/scan/${encodeURIComponent(id)}`);
  if (!response.ok) return null;
  const b = (await response.json()) as Record<string, unknown>;
  return {
    done: !!b.done,
    titlesTotal: Number(b.titles_total ?? 0),
    titlesDone: Number(b.titles_done ?? 0),
    current: String(b.current ?? ""),
    added: Array.isArray(b.added) ? (b.added as [string, number][]) : [],
    skipped: Number(b.skipped ?? 0),
    withoutLibraries: Number(b.without_libraries ?? 0),
    errors: Array.isArray(b.errors) ? (b.errors as string[]) : [],
  };
}

export interface TitleSdkPair {
  ps4: number | null;
  ps5: number | null;
  /** True when the eboot carries the FW4 pair a backport targets. Null when the
   *  eboot could not be parsed — which is NOT the same as "not backported", and
   *  must not be reported as one. */
  backported: boolean | null;
}

/** The SDK pair actually written in a title's eboot.
 *
 *  An un-backported title is indistinguishable from a wrong library set: the
 *  launch returns ok, the game dies before producing a process, and there is no
 *  missing-function line. Checking this first is the difference between "your
 *  libraries are wrong" and "this title was never patched".
 *
 *  Note it is NOT `param.json`'s `sdkVersion`, which does not change when the
 *  eboot is patched. */
export async function titleSdkPair(addr: string, source: string): Promise<TitleSdkPair | null> {
  try {
    const q = `addr=${encodeURIComponent(addr)}&path=${encodeURIComponent(source)}`;
    const response = await fetch(`${getEngineUrl()}/api/ps5/title-sdk-pair?${q}`);
    if (!response.ok) return null;
    return (await response.json()) as TitleSdkPair;
  } catch {
    return null;
  }
}
