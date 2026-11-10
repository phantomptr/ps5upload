// Shares on the PS5 (#498): the network shares PS5Upload Minion uses on the console (its Files,
// and the game Library it builds from them). They are kept on the console, so they are managed
// here per PS5; a server saved on this computer can be copied over (its password is typed again:
// the console keeps its own, encrypted).

import { useCallback, useEffect, useRef, useState } from "react";
import { HardDrive, Pencil, Plus, RefreshCw, Trash2 } from "lucide-react";

import { Button, Card, Input } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import {
  ConsoleShareError,
  listConsoleShares,
  removeConsoleShare,
  saveConsoleShare,
  type ConsoleShare,
  type ConsoleShareInput,
} from "../../api/consoleShares";
import type { Connection } from "../../api/remote";
import { useConnectionStore } from "../../state/connection";
import { useTr } from "../../state/lang";

type Tr = ReturnType<typeof useTr>;

export interface ShareForm {
  /** "" while adding. */
  id: string;
  name: string;
  server: string;
  share: string;
  start_path: string;
  user: string;
  password: string;
  domain: string;
  /** Prefilled from a server on this computer: its password must be typed again. */
  fromPc: boolean;
}

export const emptyShareForm = (): ShareForm => ({
  id: "",
  name: "",
  server: "",
  share: "",
  start_path: "",
  user: "",
  password: "",
  domain: "",
  fromPc: false,
});

export function formOfShare(s: ConsoleShare): ShareForm {
  return {
    ...emptyShareForm(),
    id: s.id,
    name: s.name,
    server: s.port && s.port !== 445 ? `${s.host}:${s.port}` : s.host,
    share: s.share,
    start_path: s.start_path,
    user: s.user,
    domain: s.domain,
  };
}

/** A server saved on this computer (SMB), as a new share for the console. */
export function formOfConnection(c: Connection): ShareForm {
  return {
    ...emptyShareForm(),
    name: c.name,
    server: c.port && c.port !== 445 ? `${c.host}:${c.port}` : c.host,
    share: c.share,
    start_path: c.start_path.replace(/^\/+|\/+$/g, ""),
    user: c.user,
    fromPc: true,
  };
}

/** What the console is sent: no password when editing and none was typed (the saved one stays). */
export function shareInput(f: ShareForm): ConsoleShareInput {
  return {
    ...(f.id ? { id: f.id } : {}),
    name: f.name.trim(),
    server: f.server.trim(),
    share: f.share.trim(),
    start_path: f.start_path.trim().replace(/^\/+|\/+$/g, ""),
    user: f.user.trim(),
    domain: f.domain.trim(),
    ...(f.id && !f.password ? {} : { password: f.password }),
  };
}

/** A refusal as one line. */
export function shareErrorText(e: unknown, tr: Tr): string {
  const reason = e instanceof ConsoleShareError ? e.reason : "";
  switch (reason) {
    case "smb_auth":
      return tr("cshares_err_auth", undefined, "The server refused that user name or password.");
    case "smb_unreachable":
    case "smb_timeout":
      return tr("cshares_err_unreachable", undefined, "The PS5 could not reach that server.");
    case "smb_no_share":
      return tr(
        "cshares_err_no_share",
        undefined,
        "The server has no share of that name. The share is the first folder name only (for example Media in \\\\nas\\Media\\Games); folders inside it go in Start folder.",
      );
    case "not_found":
    case "not_dir":
      return tr("cshares_err_folder", undefined, "That folder is not on the share.");
    case "shares_full":
      return tr("cshares_err_full", undefined, "This PS5 already keeps 16 shares. Remove one first.");
    case "bad_host":
      return tr("cshares_err_host", undefined, "That server address is not valid.");
    default:
      return e instanceof Error ? e.message : String(e);
  }
}

export function ConsoleShares({ pcServers }: { pcServers: Connection[] }) {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const up = useConnectionStore((s) => s.payloadStatus === "up");
  const { confirm, dialog } = useConfirm();
  /** null = not read yet; "unsupported" = an older helper. */
  const [shares, setShares] = useState<ConsoleShare[] | "unsupported" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [form, setForm] = useState<ShareForm | null>(null);
  const [busy, setBusy] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  // The console this card shows now. An answer from a console the user has
  // since moved away from must not land under the next one's card.
  const hostRef = useRef(host);
  useEffect(() => {
    hostRef.current = host;
  }, [host]);

  const load = useCallback(async () => {
    if (!host || !up) return;
    const asked = host;
    setError(null);
    try {
      const v = await listConsoleShares(asked);
      if (hostRef.current !== asked) return;
      setShares(v ?? "unsupported");
    } catch (e) {
      if (hostRef.current !== asked) return;
      setError(shareErrorText(e, tr));
    }
  }, [host, up, tr]);

  useEffect(() => {
    setShares(null);
    setForm(null);
    void load();
  }, [load]);

  const open = (f: ShareForm) => {
    setForm(f);
    setFormError(null);
  };

  const onSave = async () => {
    if (!form || !host) return;
    const asked = host;
    setBusy(true);
    setFormError(null);
    try {
      await saveConsoleShare(asked, shareInput(form));
      // Saved on that console; the form now belongs to another one's card.
      if (hostRef.current !== asked) return;
      setForm(null);
      await load();
    } catch (e) {
      if (hostRef.current === asked) setFormError(shareErrorText(e, tr));
    } finally {
      setBusy(false);
    }
  };

  const onRemove = async (s: ConsoleShare) => {
    if (!host) return;
    const ok = await confirm({
      title: tr("cshares_delete_title", { name: s.name }, `Remove ${s.name} from the PS5?`),
      message: tr("cshares_delete_body", undefined, "Minion stops showing it. Nothing on the server changes."),
      confirmLabel: tr("conn_delete", undefined, "Remove"),
      destructive: true,
    });
    if (!ok) return;
    try {
      await removeConsoleShare(host, s.id);
      await load();
    } catch (e) {
      setError(shareErrorText(e, tr));
    }
  };

  const smbServers = pcServers.filter((c) => c.protocol === "smb");
  const list = Array.isArray(shares) ? shares : [];
  const set = (p: Partial<ShareForm>) => setForm((f) => (f ? { ...f, ...p } : f));

  let body;
  if (!host || !up) {
    body = <p className="text-xs text-[var(--color-muted)]">{tr("cshares_need_ps5", undefined, "Connect to a PS5 to see its shares.")}</p>;
  } else if (shares === "unsupported") {
    body = (
      <p className="text-xs text-[var(--color-muted)]">
        {tr("cshares_unsupported", undefined, "Update the helper on this PS5 to manage its shares from here.")}
      </p>
    );
  } else {
    body = (
      <div className="space-y-3">
        {error ? <p className="text-xs text-[var(--color-bad)]">{error}</p> : null}
        {shares !== null && list.length === 0 ? (
          <p className="text-xs text-[var(--color-muted)]">{tr("cshares_none", undefined, "No shares on this PS5 yet.")}</p>
        ) : null}
        {list.length ? (
          <ul className="space-y-1">
            {list.map((s) => (
              <li key={s.id} className="flex flex-wrap items-center gap-2">
                <div className="min-w-0 flex-1 basis-[12rem]">
                  <div className="truncate text-sm font-medium">{s.name}</div>
                  <div className="truncate text-xs text-[var(--color-muted)]">
                    {s.host}
                    {s.port !== 445 ? `:${s.port}` : ""} / {s.share}
                    {s.start_path ? ` / ${s.start_path}` : ""}
                  </div>
                </div>
                <Button variant="ghost" size="sm" onClick={() => open(formOfShare(s))}>
                  <Pencil size={14} />
                  {tr("conn_edit", undefined, "Edit")}
                </Button>
                <Button variant="ghost" size="sm" onClick={() => void onRemove(s)}>
                  <Trash2 size={14} />
                  {tr("conn_delete", undefined, "Remove")}
                </Button>
              </li>
            ))}
          </ul>
        ) : null}
        {smbServers.length && !form ? (
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-xs text-[var(--color-muted)]">{tr("cshares_copy_from", undefined, "Copy a server from this computer:")}</span>
            {smbServers.map((c) => (
              <Button key={c.id} variant="secondary" size="sm" onClick={() => open(formOfConnection(c))}>
                {c.name}
              </Button>
            ))}
          </div>
        ) : null}
        {form ? (
          <div className="space-y-3 rounded border border-[var(--color-border)] p-3">
            <p className="text-sm font-medium">
              {form.id
                ? tr("cshares_edit_title", { name: form.name }, `Edit ${form.name} on the PS5`)
                : tr("cshares_add", undefined, "Add a share to the PS5")}
            </p>
            {form.fromPc ? (
              <p className="text-xs text-[var(--color-muted)]">
                {tr("cshares_copy_hint", undefined, "Type the password again: the PS5 keeps its own copy, encrypted.")}
              </p>
            ) : null}
            <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
              <Input label={tr("conn_name", undefined, "Name")} value={form.name} onChange={(e) => set({ name: e.target.value })} />
              <Input
                label={tr("conn_host", undefined, "Server address")}
                value={form.server}
                placeholder="192.168.1.20"
                onChange={(e) => set({ server: e.target.value })}
              />
              <Input label={tr("conn_share", undefined, "Share")} value={form.share} onChange={(e) => set({ share: e.target.value })} />
              <Input
                label={tr("conn_start_path", undefined, "Start folder (optional)")}
                value={form.start_path}
                onChange={(e) => set({ start_path: e.target.value })}
              />
              <Input label={tr("conn_user", undefined, "User")} value={form.user} onChange={(e) => set({ user: e.target.value })} />
              <Input
                label={tr("conn_password", undefined, "Password")}
                type="password"
                autoComplete="off"
                value={form.password}
                placeholder={form.id ? tr("conn_password_kept", undefined, "Saved — leave empty to keep it") : ""}
                onChange={(e) => set({ password: e.target.value })}
              />
              <Input
                label={tr("cshares_domain", undefined, "Domain (optional)")}
                value={form.domain}
                onChange={(e) => set({ domain: e.target.value })}
              />
            </div>
            {formError ? <p className="text-xs text-[var(--color-bad)]">{formError}</p> : null}
            <div className="flex gap-2">
              <Button variant="primary" size="sm" disabled={busy || !form.server.trim() || !form.share.trim()} onClick={() => void onSave()}>
                {busy ? tr("cshares_saving", undefined, "The PS5 is signing in…") : tr("cshares_save", undefined, "Sign in and save")}
              </Button>
              <Button variant="ghost" size="sm" disabled={busy} onClick={() => setForm(null)}>
                {tr("cancel", undefined, "Cancel")}
              </Button>
            </div>
          </div>
        ) : null}
      </div>
    );
  }

  return (
    <Card
      className="mt-6"
      title={tr("cshares_title", undefined, "Shares on the PS5")}
      right={
        host && up && shares !== "unsupported" ? (
          <div className="flex flex-wrap justify-end gap-1">
            <Button variant="ghost" size="sm" onClick={() => void load()}>
              <RefreshCw size={14} />
              {tr("refresh", undefined, "Refresh")}
            </Button>
            <Button variant="secondary" size="sm" disabled={!!form} onClick={() => open(emptyShareForm())}>
              <Plus size={14} />
              {tr("cshares_add", undefined, "Add a share to the PS5")}
            </Button>
          </div>
        ) : undefined
      }
    >
      {dialog}
      <p className="mb-3 flex items-start gap-2 text-xs text-[var(--color-muted)]">
        <HardDrive size={14} className="mt-0.5 shrink-0" />
        {tr(
          "cshares_desc",
          undefined,
          "PS5Upload Minion on the console uses these: it browses them in Files, and its Library shows the games on them, to install or copy from the TV. They are kept on the PS5.",
        )}
      </p>
      {body}
    </Card>
  );
}
