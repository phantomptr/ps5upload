// What a package, image or game folder is — the engine's /api/game/inspect —
// for the package viewer. Field names match the engine's JSON (snake_case).
import { invoke } from "../lib/invokeLogged";

export interface GameInspection {
  source: {
    format: string;
    location: string;
    path: string;
    size: number;
    parts: { path: string; size: number }[];
  };
  identity: {
    title: string;
    titles: Record<string, string>;
    title_id: string;
    content_id: string;
    concept_id: string | null;
    platform: string;
    /** `ps4` / `ps5` from eboot.bin's process param (its SDK words); `platform` falls back to
     *  it when the title id says nothing. Absent from an older engine. */
    exe_platform?: string | null;
    category: string;
    content_type: string;
    region: string | null;
  };
  specs: {
    app_ver: string;
    master_ver: string | null;
    min_fw: string | null;
    sdk_ver: string | null;
    build_date: string | null;
    drm: string | null;
    age_rating: string | null;
    languages: string[];
    file_count: number | null;
  };
  params: { key: string; value: string }[];
  change_notes: string | null;
  images: { name: string; size: number }[];
  authenticity: string;
  warnings: string[];
  partial: boolean;
}

export function gameInspect(path: string) {
  return invoke<{ token: string; inspection: GameInspection }>("game_inspect", {
    path,
  });
}

export interface InspectFile {
  path: string;
  size: number;
  encrypted: boolean;
}

/** The Files tab: a package's entries, or the files of a folder or image. */
export function gameInspectFiles(token: string) {
  return invoke<{ files: InspectFile[]; truncated: boolean }>("game_inspect_files", { token });
}

/** An inspected source's image as a `data:` URL, in every build. */
export async function gameInspectImageUrl(
  token: string,
  name: string,
): Promise<string> {
  const r = await invoke<{ base64: string }>("game_inspect_image", {
    token,
    name,
  });
  return `data:image/png;base64,${r.base64}`;
}
