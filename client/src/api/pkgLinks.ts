// "Copy install link": packages the engine hosts for a console that fetches them itself
// (engine/crates/ps5upload-engine/src/pkg_install.rs, /api/pkg/links).

import { getEngineUrl } from "../state/engine";

export interface SharedLink {
  id: string;
  url: string;
  title: string;
  content_id: string;
  total_size: number;
  requests_served: number;
  transfer_bytes: number;
  created_at_unix: number;
  last_activity_unix: number;
  /** Any device on the network may fetch it, not only the console it was made for. */
  any_device: boolean;
}

async function call<T>(path: string, body?: unknown): Promise<T> {
  const res = await fetch(`${getEngineUrl()}/api/pkg/links${path}`, {
    method: body === undefined ? "GET" : "POST",
    headers:
      body === undefined ? undefined : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  let parsed: unknown;
  try {
    parsed = text ? JSON.parse(text) : null;
  } catch {
    parsed = null;
  }
  if (!res.ok) {
    throw new Error(
      (parsed as { error?: string } | null)?.error ?? `HTTP ${res.status}`,
    );
  }
  return parsed as T;
}

export const pkgLinks = {
  list: () => call<SharedLink[]>(""),
  /** Hosts `path` for the console at `ps5Addr`; an existing link for it is returned. */
  create: (ps5Addr: string, path: string) =>
    call<SharedLink>("", { ps5_addr: ps5Addr, path }),
  stop: (id: string) => call<{ ok: boolean }>("/stop", { id }),
  open: (id: string, any: boolean) => call<SharedLink>("/open", { id, any }),
};
