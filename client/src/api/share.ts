// Share (engine share/mod.rs): an app's things sent to a friend. The sender makes a share and
// gets a link; the friend's engine opens it, accepts some of what is offered; the sender allows
// or declines; the files then go straight from one computer to the other. The engine refuses a
// retail game whatever this page sends.

import { getEngineUrl } from "../state/engine";
import { ENGINE_LONG_TIMEOUT_MS, engineFetch } from "../lib/engineFetch";

export type ShareKind = "app" | "save" | "media" | "cheats" | "controls" | "stream" | "note";

/** One thing to share, as POST /api/share takes it. */
export interface ShareItemSpec {
  kind: ShareKind;
  /** The console it is on (an app folder, a save, screenshots). */
  console?: string;
  /** On the console when `console` is given, else on the engine's computer. */
  path?: string;
  paths?: string[];
  folder?: string;
  name?: string;
  data?: unknown;
  text?: string;
}

export interface ShareCreate {
  from?: string;
  title?: string;
  title_id?: string;
  expires_in_s?: number;
  one_use?: boolean;
  items: ShareItemSpec[];
}

export interface ShareItem {
  id: string;
  kind: ShareKind;
  name: string;
  title_id: string | null;
  format: string | null;
  size: number;
  files: number;
  /** The app's: how it was built (`marks`: the store's build marks found; none for homebrew). */
  build?: { read: boolean; marks: string[] } | null;
}

export type ShareRequestState =
  | "asking"
  | "allowed"
  | "sending"
  | "done"
  | "declined"
  | "cancelled"
  | "left"
  | "failed";

export interface ShareRequest {
  request: number;
  name: string;
  wants: string[];
  wants_names: string[];
  state: ShareRequestState;
  bytes_done: number;
  bytes_total: number;
  why: string | null;
}

export interface ShareView {
  id: string;
  /** open | used | expired | cancelled */
  state: string;
  from: string;
  title: string;
  title_id: string | null;
  one_use: boolean;
  expires_in_s: number;
  url: string | null;
  lan_url: string | null;
  /** "anywhere": the internet link (works on the Wi-Fi too); "wifi": the home network's only. */
  works: "anywhere" | "wifi";
  items: ShareItem[];
  requests: ShareRequest[];
}

export type ReceiveState =
  | "connecting"
  | "offered"
  | "asking"
  | "receiving"
  | "reconnecting"
  | "done"
  | "declined"
  | "cancelled"
  | "failed"
  | "refused";

export interface OfferedItem {
  id: string;
  kind: ShareKind;
  name: string;
  title_id: string | null;
  format: string | null;
  size: number;
  files: number;
  /** Why this side won't take it (a retail app). */
  refused: string | null;
}

export interface ReceiveView {
  id: string;
  state: ReceiveState;
  from: string;
  title: string;
  title_id: string | null;
  items: OfferedItem[];
  accepted: string[];
  expires_in_s: number | null;
  bytes_done: number;
  bytes_total: number;
  error: string | null;
  /** lan | tailscale | internet */
  path: string | null;
  dest: string | null;
  /** Where each item now is; `file`: a one-file app's package or image, once done. */
  received: { item: string; kind: ShareKind | null; path: string; file?: string | null }[];
}

export interface ShareHistoryEntry {
  at_ms: number;
  dir: "sent" | "received";
  name: string;
  items: string[];
  state: string;
}

export interface ShareList {
  shares: ShareView[];
  receives: ReceiveView[];
  history: ShareHistoryEntry[];
}

export interface ShareCheckItem {
  item: number;
  ok: boolean;
  error?: string;
  code?: "retail" | "item";
  title_id?: string | null;
}

/** A refusal from the engine: `code` "retail" for a commercial game, with its Title ID. */
export class ShareError extends Error {
  code: string | null;
  titleId: string | null;
  constructor(message: string, code: string | null = null, titleId: string | null = null) {
    super(message);
    this.name = "ShareError";
    this.code = code;
    this.titleId = titleId;
  }
}

async function failed(r: Response): Promise<never> {
  const b = (await r.json().catch(() => ({}))) as { error?: string; code?: string; title_id?: string };
  throw new ShareError(b.error || `HTTP ${r.status}`, b.code ?? null, b.title_id ?? null);
}

async function call<T>(method: string, path: string, body?: unknown, long = false): Promise<T> {
  const r = await engineFetch(`${getEngineUrl()}${path}`, {
    method,
    ...(long ? { timeoutMs: ENGINE_LONG_TIMEOUT_MS } : {}),
    ...(body === undefined
      ? {}
      : { headers: { "content-type": "application/json" }, body: JSON.stringify(body) }),
  });
  if (!r.ok) return failed(r);
  return (await r.json()) as T;
}

const enc = encodeURIComponent;

/** Makes a share (an app is read and checked by the engine; a save is packed: slow). */
export async function shareCreate(body: ShareCreate): Promise<ShareView> {
  return call<ShareView>("POST", "/api/share", body, true);
}

/** Each item's verdict without making a share. */
export async function shareCheck(body: ShareCreate): Promise<ShareCheckItem[]> {
  const r = await call<{ items: ShareCheckItem[] }>("POST", "/api/share/check", body, true);
  return r.items ?? [];
}

export async function shareList(): Promise<ShareList> {
  const r = await call<Partial<ShareList>>("GET", "/api/share");
  return { shares: r.shares ?? [], receives: r.receives ?? [], history: r.history ?? [] };
}

export async function shareGet(id: string): Promise<ShareView> {
  return call<ShareView>("GET", `/api/share/${enc(id)}`);
}

export async function shareDecide(id: string, request: number, allow: boolean): Promise<ShareView> {
  return call<ShareView>("POST", `/api/share/${enc(id)}/decide`, { request, allow });
}

export async function shareCancel(id: string): Promise<ShareView> {
  return call<ShareView>("DELETE", `/api/share/${enc(id)}`);
}

/** Opens a Share link on this engine; nothing is received until `receiveAccept`. */
export async function receiveOpen(link: string, name: string): Promise<string> {
  const r = await call<{ id: string }>("POST", "/api/share/receive", { link, name });
  return r.id;
}

export async function receiveGet(id: string): Promise<ReceiveView> {
  return call<ReceiveView>("GET", `/api/share/receive/${enc(id)}`);
}

export async function receiveAccept(id: string, items: string[], dest?: string): Promise<void> {
  await call<unknown>("POST", `/api/share/receive/${enc(id)}/accept`, dest ? { items, dest } : { items });
}

export async function receiveCancel(id: string): Promise<void> {
  await call<unknown>("DELETE", `/api/share/receive/${enc(id)}`);
}

/** A received save into a user's own save of that game (backed up first). The parts replaced. */
export async function receiveUseSave(
  id: string,
  item: string,
  addr: string,
  folder: string,
  userId?: number,
): Promise<string[]> {
  const r = await call<{ parts?: string[] }>(
    "POST",
    `/api/share/receive/${enc(id)}/use-save`,
    { addr, folder, item, ...(userId === undefined ? {} : { user_id: userId >>> 0 }) },
    true,
  );
  return r.parts ?? [];
}

export interface ReceivedData {
  kind: ShareKind | null;
  path: string;
  files: Record<string, unknown>;
}

/** A received small item (controls, stream settings, which cheats are on, a note). */
export async function receiveData(id: string, item: string): Promise<ReceivedData> {
  return call<ReceivedData>("GET", `/api/share/receive/${enc(id)}/data/${enc(item)}`);
}

/** Whether `text` looks like a Share link (the website's or a home network's). */
export function isShareLink(text: string): boolean {
  const t = text.trim();
  return /^https?:\/\/[^#\s]+\/share\/?#/.test(t) && /[#&]room=[A-Za-z0-9_-]{8,64}/.test(t);
}
