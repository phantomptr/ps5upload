// Starting a link download on the engine (`POST /api/pkg/remote/download/start`), with its
// two refusals kept apart instead of flattened into one error string:
//  - the same file is already downloading: its id comes back, so the caller watches that
//    download instead of failing;
//  - a different file already has the name: the caller asks whether to resume into it or
//    replace it, and starts again with that choice.
// Called over the engine's HTTP API directly (both builds may reach it), because the Tauri
// command path keeps only the error text and drops the id and sizes.

import { getEngineUrl } from "../state/engine";

export type ExistingChoice = "resume" | "replace";

export type LinkDownloadStart =
  | { kind: "started"; id: string; path: string; total: number }
  /** The same file is already downloading: watch this one. */
  | { kind: "attached"; id: string }
  /** A different file has the name: resume into it or replace it. */
  | { kind: "exists"; path: string; existingBytes: number; total: number; message: string };

export interface LinkDownloadArgs {
  url: string;
  insecureTls: boolean;
  destDir?: string | null;
  keepName?: boolean;
  existing?: ExistingChoice;
}

type Fetch = (input: string, init?: RequestInit) => Promise<Response>;

export async function startLinkDownload(
  args: LinkDownloadArgs,
  fetchImpl: Fetch = (i, init) => fetch(i, init),
): Promise<LinkDownloadStart> {
  const res = await fetchImpl(`${getEngineUrl()}/api/pkg/remote/download/start`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      url: args.url,
      insecure_tls: args.insecureTls,
      dest_dir: args.destDir ?? null,
      keep_name: args.keepName ?? false,
      ...(args.existing ? { existing: args.existing } : {}),
    }),
  });
  const text = await res.text().catch(() => "");
  let body: Record<string, unknown> = {};
  try {
    body = text ? (JSON.parse(text) as Record<string, unknown>) : {};
  } catch {
    /* not JSON: the status line explains it below */
  }
  const str = (k: string) => (typeof body[k] === "string" ? (body[k] as string) : "");
  const num = (k: string) => (typeof body[k] === "number" ? (body[k] as number) : 0);
  if (res.ok) {
    if (!str("download_id") || !str("path"))
      throw new Error("The engine did not start a download for that link.");
    return { kind: "started", id: str("download_id"), path: str("path"), total: num("total") };
  }
  if (res.status === 409 && str("download_id")) return { kind: "attached", id: str("download_id") };
  if (res.status === 409 && str("existing_path")) {
    return {
      kind: "exists",
      path: str("existing_path"),
      existingBytes: num("existing_bytes"),
      total: num("total"),
      message: str("error"),
    };
  }
  throw new Error(str("error") || text.trim() || `engine HTTP ${res.status}`);
}

/** Resume is offered only for a file smaller than the link: an earlier unfinished download
 *  of it is the likely story. A bigger file is not this package cut short. */
export function canResumeExisting(existingBytes: number, total: number): boolean {
  return existingBytes > 0 && total > 0 && existingBytes < total;
}
