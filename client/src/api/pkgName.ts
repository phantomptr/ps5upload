// The name a fake package gives its game (#937): the engine's /api/pkg/name reads it and
// changes it in the package file on the engine's computer
// (engine/crates/ps5upload-engine/src/pkg_name_api.rs). Plain fetches against the engine origin,
// so the desktop app, the browser build and the Docker build take the same path.

import { getEngineUrl } from "../state/engine";
import { ENGINE_LONG_TIMEOUT_MS, engineFetch } from "../lib/engineFetch";

/** Why a package's name can't be changed. `code`: `retail` (an official package), `patch`
 *  (a delta update), `no_name`, or `unsupported` with the engine's reason in `reason`. */
export interface PkgNameRefusal {
  code: string;
  reason: string;
}

export interface PkgNames {
  platform: "ps5" | "ps4";
  content_id: string;
  /** The name the console shows unless its language has another. */
  name: string;
  /** One per language (PS5: `en-US`…; PS4: `TITLE`, `TITLE_03`…). */
  entries: { key: string; name: string }[];
  /** How many entries hold another name than `name`. */
  other_names: number;
  refusal?: PkgNameRefusal | null;
  /** The longest name accepted, in UTF-8 bytes. */
  max_bytes: number;
  /** How long a name this package has room for, in UTF-8 bytes (a rename moves nothing in the
   *  package, so the room is its own): for the default name alone, and for every language.
   *  Absent when the name can't be changed, and from an engine older than 12.6.32. */
  room?: { default_name: number; all_languages: number } | null;
}

export interface PkgRenamed {
  platform: "ps5" | "ps4";
  previous: string;
  name: string;
  /** Name entries changed; 0 when it already had this name. */
  changed: number;
  written_bytes: number;
  warning?: string;
}

async function answer<T>(res: Response): Promise<T> {
  const text = await res.text();
  let parsed: unknown;
  try {
    parsed = text ? JSON.parse(text) : null;
  } catch {
    parsed = null;
  }
  if (!res.ok) {
    throw new Error((parsed as { error?: string } | null)?.error ?? `HTTP ${res.status}`);
  }
  return parsed as T;
}

/** The names the package at `path` (on the engine's computer) holds, and whether they can be
 *  changed. Reads the package's container, which takes a moment for a large package. */
export async function readPkgName(path: string): Promise<PkgNames> {
  const res = await engineFetch(
    `${getEngineUrl()}/api/pkg/name?path=${encodeURIComponent(path)}`,
    { timeoutMs: ENGINE_LONG_TIMEOUT_MS },
  );
  const names = await answer<PkgNames>(res);
  // An engine from before this route answers something else: no names, not a broken field.
  if (!names || typeof names.name !== "string" || !Array.isArray(names.entries)) {
    throw new Error("the engine did not say what the package is named");
  }
  return names;
}

/** Gives the package at `path` the name `name`, in the file itself. Throws with the engine's
 *  reason when it can't (in use, an official package, a name that does not fit); the file is
 *  then as it was. */
export async function setPkgName(
  path: string,
  name: string,
  allLanguages: boolean,
): Promise<PkgRenamed> {
  const res = await engineFetch(`${getEngineUrl()}/api/pkg/name`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ path, name, all_languages: allLanguages }),
    timeoutMs: ENGINE_LONG_TIMEOUT_MS,
  });
  return answer<PkgRenamed>(res);
}
