// Link classification + download-only (R4, #368) and RAR package routes (R6, #370).
//
// Kept out of `ps5.ts`: these four calls are the whole surface the two features add, and
// a separate module keeps the (very large) ps5 api mock in the older store tests untouched.

import { invoke } from "../lib/invokeLogged";

export type LinkKind = "pkg" | "file" | "refused";

/** What the engine found at the end of a link's redirects. Decided from the response (the
 *  package magic, the status, the content headers), never from the URL's spelling. */
export interface LinkClass {
  kind: LinkKind;
  /** Only for `refused`: `html`, `login`, `not_found`, `status`, `empty`, `not_a_file`. */
  reason?: string;
  /** Only for `refused`: a sentence for the user. */
  message?: string;
  /** Name from Content-Disposition, else the final URL. Never empty unless refused. */
  filename: string;
  total_size: number | null;
  /** The server honours byte ranges, which streaming to the console needs. */
  ranges: boolean;
  content_type: string;
}

export async function linkProbe(
  url: string,
  insecureTls = false,
): Promise<LinkClass> {
  return invoke<LinkClass>("link_probe", {
    url,
    insecure_tls: insecureTls,
  });
}

/** Stream a link's file to `destDir` on the console over AVA1 (no local copy). Returns the
 *  job id; follow it with `waitForJob`/`jobStatus`. The engine probes the link again and
 *  refuses anything that is not a real file download. */
export async function startLinkDownload(opts: {
  url: string;
  destDir: string;
  addr: string;
  fileName?: string | null;
  insecureTls?: boolean;
}): Promise<string> {
  const res = await invoke<{ job_id: string }>("link_download", {
    req: {
      url: opts.url,
      dest_dir: opts.destDir,
      addr: opts.addr,
      file_name: opts.fileName ?? null,
      insecure_tls: opts.insecureTls ?? false,
    },
  });
  return res.job_id;
}

export interface RarPackage {
  /** Path inside the archive ('/'-separated). */
  path: string;
  size: number;
}

/** The `.pkg` entries of a RAR (any folder depth), from the headers alone. Throws with
 *  `rar_password_required` / `rar_password_wrong` in the message when it needs a password. */
export async function rarPackages(
  archivePath: string,
  password?: string | null,
): Promise<RarPackage[]> {
  const res = await invoke<{ packages: RarPackage[] }>("rar_packages", {
    req: { archive_path: archivePath, password: password ?? null },
  });
  return res.packages ?? [];
}

export interface ConsolePkgInfo {
  total_size: number;
  filename: string;
  content_id: string;
  title: string;
  title_id: string;
  /** PARAM.SFO category: `gd` base, `gp` patch, `ac` DLC. */
  category: string;
  app_ver: string;
  platform: string;
  package_type: string;
}

/** Identify a package already on the console by reading its header over the helper. */
export async function pkgConsoleProbe(
  host: string,
  path: string,
): Promise<ConsolePkgInfo> {
  return invoke<ConsolePkgInfo>("pkg_console_probe", { host, path });
}
