// The engine's /api/remote/* routes: saved servers and what is on them. Called with fetch
// against the engine URL, so the desktop app, Android and the web build share one path.

import { getEngineUrl } from "../state/engine";

export type Protocol = "smb" | "ftp" | "ftps" | "sftp";

export interface Connection {
  id: string;
  name: string;
  protocol: Protocol;
  host: string;
  port: number;
  share: string;
  user: string;
  start_path: string;
  host_key: string | null;
  has_secret: boolean;
}

export interface ConnectionInput {
  name: string;
  protocol: Protocol;
  host: string;
  port: number;
  share?: string;
  user?: string;
  start_path?: string;
}

export interface SecretInput {
  password?: string;
  key_pem?: string;
  key_passphrase?: string;
}

export interface RemoteEntry {
  name: string;
  is_dir: boolean;
  size: number;
  mtime: number | null;
}

export interface RemoteFailure {
  error: string;
  hint?: string | null;
  host_key?: string | null;
}

export type TestResult = { ok: true } | ({ ok: false } & RemoteFailure);

/** An unsaved form; `id` (editing) lets the engine borrow the saved secret when none is typed. */
export type FormBody = { id?: string; connection: ConnectionInput } & SecretInput;

/** A failed call, carrying the engine's plain-language hint when it has one. */
export class RemoteApiError extends Error {
  hint?: string;
  hostKey?: string;
  constructor(message: string, hint?: string | null, hostKey?: string | null) {
    super(message);
    this.hint = hint ?? undefined;
    this.hostKey = hostKey ?? undefined;
  }
}

async function call<T>(method: string, path: string, body?: unknown): Promise<T> {
  const res = await fetch(`${getEngineUrl()}/api/remote${path}`, {
    method,
    headers: body === undefined ? undefined : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  let json: unknown = null;
  try {
    json = text ? JSON.parse(text) : null;
  } catch {
    /* a non-JSON error body is reported by its text below */
  }
  if (!res.ok) {
    const f = (json ?? {}) as Partial<RemoteFailure>;
    throw new RemoteApiError(f.error ?? (text || `HTTP ${res.status}`), f.hint, f.host_key);
  }
  return json as T;
}

const idPath = (id: string) => `/connections/${encodeURIComponent(id)}`;

export const remoteApi = {
  async list(): Promise<Connection[]> {
    return (await call<{ connections: Connection[] }>("GET", "/connections")).connections;
  },
  add(c: ConnectionInput, s: SecretInput = {}): Promise<Connection> {
    return call("POST", "/connections", { connection: c, ...s });
  },
  update(id: string, c: ConnectionInput, s: SecretInput = {}): Promise<Connection> {
    return call("PUT", idPath(id), { connection: c, ...s });
  },
  async remove(id: string): Promise<void> {
    await call("DELETE", idPath(id));
  },
  test(idOrForm: string | FormBody): Promise<TestResult> {
    return typeof idOrForm === "string"
      ? call("POST", `${idPath(idOrForm)}/test`)
      : call("POST", "/test", idOrForm);
  },
  async shares(idOrForm: string | FormBody): Promise<{ name: string; comment: string }[]> {
    const r =
      typeof idOrForm === "string"
        ? await call<{ shares: { name: string; comment: string }[] }>("GET", `${idPath(idOrForm)}/shares`)
        : await call<{ shares: { name: string; comment: string }[] }>("POST", "/shares", idOrForm);
    return r.shares;
  },
  listDir(path: string, cursor?: string): Promise<{ entries: RemoteEntry[]; next_cursor: string | null }> {
    return call("POST", "/list", { path, cursor: cursor ?? null });
  },
  fetch(path: string, destDir?: string): Promise<{ job_id: string }> {
    return call("POST", "/fetch", { path, dest_dir: destDir ?? null });
  },
  async cleanupFetched(dest: string): Promise<void> {
    await call("POST", "/fetch/cleanup", { dest });
  },
  async acceptHostKey(id: string, fingerprint: string): Promise<void> {
    await call("POST", `${idPath(id)}/host-key`, { fingerprint });
  },
};
