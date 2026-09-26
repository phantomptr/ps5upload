// Saved servers: add, test, edit, delete, and browse. Every "Browse…" in the app that reads a
// source lists these in its ▾ menu.

import { useCallback, useEffect, useRef, useState } from "react";
import { useNavigate, useSearchParams } from "react-router";
import { FolderOpen, Network, Pencil, Plus, RefreshCw, Trash2 } from "lucide-react";

import { Button, Card, ErrorCard, PageHeader } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import { remoteApi, type Connection, type TestResult } from "../../api/remote";
import { useConnectionsStore, type ConnStatus } from "../../state/connections";
import { useTr } from "../../state/lang";
import { browseServer } from "../../lib/browseServer";
import {
  ConnectionFormView,
  DEFAULT_PORT,
  emptyForm,
  toRequest,
  type FormValue,
} from "./ConnectionForm";

const PROTOCOL: Record<Connection["protocol"], string> = {
  smb: "SMB",
  ftp: "FTP",
  ftps: "FTPS",
  sftp: "SFTP",
};

function formOf(c: Connection): FormValue {
  return {
    ...emptyForm(),
    name: c.name,
    protocol: c.protocol,
    host: c.host,
    port: c.port || DEFAULT_PORT[c.protocol],
    share: c.share,
    guest: !c.user,
    user: c.user,
    startPath: c.start_path,
  };
}

function StatusDot({ status }: { status: ConnStatus | undefined }) {
  const tr = useTr();
  const [color, text] =
    status === "reachable"
      ? ["bg-[var(--color-good)]", tr("conn_status_ok", undefined, "Reachable")]
      : status === "auth-failed"
        ? ["bg-[var(--color-bad)]", tr("conn_status_auth", undefined, "Sign-in failed")]
        : status === "offline"
          ? ["bg-[var(--color-muted)]", tr("conn_status_offline", undefined, "Offline")]
          : ["bg-[var(--color-border)]", tr("conn_status_unknown", undefined, "Not checked")];
  return (
    <span className="inline-flex items-center gap-1.5 text-xs text-[var(--color-muted)]">
      <span className={`h-2 w-2 rounded-full ${color}`} aria-hidden />
      {text}
    </span>
  );
}

export default function ConnectionsScreen() {
  const tr = useTr();
  const navigate = useNavigate();
  const [params, setParams] = useSearchParams();
  const { confirm, dialog } = useConfirm();
  const connections = useConnectionsStore((s) => s.connections);
  const loaded = useConnectionsStore((s) => s.loaded);
  const status = useConnectionsStore((s) => s.status);
  const load = useConnectionsStore((s) => s.load);
  const save = useConnectionsStore((s) => s.save);
  const remove = useConnectionsStore((s) => s.remove);
  const check = useConnectionsStore((s) => s.check);

  const [error, setError] = useState<string | null>(null);
  /** null = form closed; "" = adding; otherwise the id being edited. */
  const [editing, setEditing] = useState<string | null>(null);
  const [form, setForm] = useState<FormValue>(emptyForm());
  const [shares, setShares] = useState<string[]>([]);
  const [testResult, setTestResult] = useState<TestResult | null>(null);
  const [busy, setBusy] = useState(false);
  const keyInput = useRef<HTMLInputElement>(null);

  const refresh = useCallback(async () => {
    setError(null);
    try {
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [load]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // Check every server once the list is in, so the dots mean something.
  useEffect(() => {
    if (!loaded) return;
    for (const c of useConnectionsStore.getState().connections) void check(c.id);
  }, [loaded, check]);

  const openForm = useCallback((c: Connection | null) => {
    setEditing(c ? c.id : "");
    setForm(c ? formOf(c) : emptyForm());
    setShares([]);
    setTestResult(null);
  }, []);

  // /connections?add=1 and ?edit=<id> come from the Browse menu, Home and the picker.
  useEffect(() => {
    if (!loaded) return;
    const edit = params.get("edit");
    if (params.get("add")) openForm(null);
    else if (edit) openForm(connections.find((c) => c.id === edit) ?? null);
    else return;
    setParams({}, { replace: true });
  }, [loaded, params, connections, openForm, setParams]);

  const current = editing ? connections.find((c) => c.id === editing) : undefined;
  const formBody = () => {
    const { connection, secret } = toRequest(form);
    return { ...(editing ? { id: editing } : {}), connection, ...secret };
  };

  const onChange = (v: FormValue) => {
    setForm(v);
    setTestResult(null); // any edit needs a fresh test before Save
  };

  const onTest = async () => {
    setBusy(true);
    try {
      setTestResult(await remoteApi.test(formBody()));
    } catch (e) {
      setTestResult({ ok: false, error: e instanceof Error ? e.message : String(e) });
    } finally {
      setBusy(false);
    }
  };

  const onSave = async () => {
    setBusy(true);
    try {
      const { connection, secret } = toRequest(form);
      const saved = await save(editing || null, connection, secret);
      setEditing(null);
      void check(saved.id);
    } catch (e) {
      setTestResult({ ok: false, error: e instanceof Error ? e.message : String(e) });
    } finally {
      setBusy(false);
    }
  };

  /** Trusting a server's key needs a saved connection to keep it on. */
  const onAcceptHostKey = async (fingerprint: string) => {
    setBusy(true);
    try {
      const { connection, secret } = toRequest(form);
      const id = editing || (await save(null, connection, secret)).id;
      setEditing(id);
      await remoteApi.acceptHostKey(id, fingerprint);
      setTestResult(await remoteApi.test(id));
    } catch (e) {
      setTestResult({ ok: false, error: e instanceof Error ? e.message : String(e) });
    } finally {
      setBusy(false);
    }
  };

  const onListShares = async () => {
    try {
      setShares((await remoteApi.shares(formBody())).map((s) => s.name));
    } catch (e) {
      setTestResult({ ok: false, error: e instanceof Error ? e.message : String(e) });
    }
  };

  const onKeyFile = async (file: File | undefined) => {
    if (!file) return;
    const pem = await file.text();
    onChange({ ...form, keyPem: pem, keyName: file.name });
  };

  const onDelete = async (c: Connection) => {
    const ok = await confirm({
      title: tr("conn_delete_title", { name: c.name }, `Remove ${c.name}?`),
      message: tr(
        "conn_delete_body",
        undefined,
        "Its saved password or key is deleted too. Nothing on the server changes.",
      ),
      confirmLabel: tr("conn_delete", undefined, "Remove"),
      destructive: true,
    });
    if (!ok) return;
    try {
      await remove(c.id);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const browse = (c: Connection) =>
    browseServer(c, navigate, tr("conn_need_ps5", undefined, "Connect to a PS5 to install."));

  return (
    <div className="p-6">
      {dialog}
      <PageHeader
        icon={Network}
        title={tr("connections_title", undefined, "Connections")}
        description={tr(
          "connections_subtitle",
          undefined,
          "Your NAS and servers (SMB, FTP, SFTP). Anywhere you browse for a game or file, pick from them with the ▾ next to Browse.",
        )}
        right={
          <div className="flex gap-2">
            <Button variant="ghost" onClick={() => void refresh()}>
              <RefreshCw size={16} />
              {tr("refresh", undefined, "Refresh")}
            </Button>
            <Button variant="primary" onClick={() => openForm(null)}>
              <Plus size={16} />
              {tr("conn_add", undefined, "Add a server")}
            </Button>
          </div>
        }
      />
      {error && (
        <div className="mb-4">
          <ErrorCard title={error} />
        </div>
      )}

      {editing !== null && (
        <Card
          className="mb-4"
          title={
            editing
              ? tr("conn_edit_title", { name: current?.name ?? "" }, `Edit ${current?.name ?? ""}`)
              : tr("conn_add_title", undefined, "Add a server")
          }
        >
          <ConnectionFormView
            value={form}
            onChange={onChange}
            shares={shares}
            testResult={testResult}
            busy={busy}
            hasSavedSecret={!!current?.has_secret}
            onTest={() => void onTest()}
            onSave={() => void onSave()}
            onCancel={() => setEditing(null)}
            onAcceptHostKey={(fp) => void onAcceptHostKey(fp)}
            onPickKeyFile={() => keyInput.current?.click()}
            onListShares={() => void onListShares()}
          />
          <input
            ref={keyInput}
            type="file"
            className="hidden"
            onChange={(e) => void onKeyFile(e.target.files?.[0])}
          />
        </Card>
      )}

      {loaded && connections.length === 0 && editing === null ? (
        <Card>
          <div className="flex flex-col items-center gap-3 py-8 text-center">
            <Network size={28} className="text-[var(--color-muted)]" />
            <p className="text-sm font-medium">
              {tr("conn_empty_title", undefined, "No servers yet")}
            </p>
            <p className="max-w-md text-xs text-[var(--color-muted)]">
              {tr(
                "conn_empty_body",
                undefined,
                "Add your NAS, a shared folder on a PC, or an FTP/SFTP server, and pick games straight from it.",
              )}
            </p>
            <Button variant="primary" onClick={() => openForm(null)}>
              <Plus size={16} />
              {tr("conn_add", undefined, "Add a server")}
            </Button>
          </div>
        </Card>
      ) : (
        <ul className="space-y-2">
          {connections.map((c) => (
            <li key={c.id}>
              <Card>
                <div className="flex flex-wrap items-center gap-3">
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2">
                      <span className="truncate font-medium">{c.name}</span>
                      <span className="rounded bg-[var(--color-surface-3)] px-1.5 text-xs text-[var(--color-muted)]">
                        {PROTOCOL[c.protocol]}
                      </span>
                    </div>
                    <div className="truncate text-xs text-[var(--color-muted)]">
                      {c.host}
                      {c.port !== DEFAULT_PORT[c.protocol] ? `:${c.port}` : ""}
                      {c.share ? ` / ${c.share}` : ""}
                    </div>
                  </div>
                  <StatusDot status={status[c.id]} />
                  <div className="flex flex-wrap gap-1">
                    <Button variant="secondary" size="sm" onClick={() => browse(c)}>
                      <FolderOpen size={14} />
                      {tr("conn_browse", undefined, "Browse")}
                    </Button>
                    <Button variant="ghost" size="sm" onClick={() => void check(c.id)}>
                      {tr("conn_test", undefined, "Test")}
                    </Button>
                    <Button variant="ghost" size="sm" onClick={() => openForm(c)}>
                      <Pencil size={14} />
                      {tr("conn_edit", undefined, "Edit")}
                    </Button>
                    <Button variant="ghost" size="sm" onClick={() => void onDelete(c)}>
                      <Trash2 size={14} />
                      {tr("conn_delete", undefined, "Remove")}
                    </Button>
                  </div>
                </div>
              </Card>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
