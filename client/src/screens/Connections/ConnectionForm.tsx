// The add/edit form for a saved server. Pure: the screen owns the state and the engine calls.

import { AlertTriangle, CheckCircle2, KeyRound, ShieldAlert, XCircle } from "lucide-react";

import { Button, Input, SegmentedControl, Toggle } from "../../components";
import type { ConnectionInput, Protocol, SecretInput, TestResult } from "../../api/remote";
import { useTr } from "../../state/lang";

export interface FormValue {
  name: string;
  protocol: Protocol;
  host: string;
  port: number;
  share: string;
  guest: boolean;
  user: string;
  authKind: "password" | "key";
  password: string;
  keyPem: string;
  keyName: string;
  keyPassphrase: string;
  startPath: string;
}

export const DEFAULT_PORT: Record<Protocol, number> = { smb: 445, ftp: 21, ftps: 21, sftp: 22 };

export function emptyForm(): FormValue {
  return {
    name: "",
    protocol: "smb",
    host: "",
    port: 445,
    share: "",
    guest: false,
    user: "",
    authKind: "password",
    password: "",
    keyPem: "",
    keyName: "",
    keyPassphrase: "",
    startPath: "",
  };
}

/** A new protocol brings its default port, unless the user had typed their own. */
export function switchProtocol(v: FormValue, protocol: Protocol): FormValue {
  const port = v.port === DEFAULT_PORT[v.protocol] ? DEFAULT_PORT[protocol] : v.port;
  return { ...v, protocol, port, authKind: protocol === "sftp" ? v.authKind : "password" };
}

/** The form as the engine takes it. An empty secret on an edit keeps the saved one. */
export function toRequest(v: FormValue): { connection: ConnectionInput; secret: SecretInput } {
  const connection: ConnectionInput = {
    name: v.name.trim(),
    protocol: v.protocol,
    host: v.host.trim(),
    port: v.port,
    share: v.protocol === "smb" ? v.share.trim() : "",
    user: v.guest ? "" : v.user.trim(),
    start_path: v.startPath.trim(),
  };
  const secret: SecretInput = {};
  if (!v.guest) {
    if (v.protocol === "sftp" && v.authKind === "key") {
      if (v.keyPem) secret.key_pem = v.keyPem;
      if (v.keyPassphrase) secret.key_passphrase = v.keyPassphrase;
    } else if (v.password) {
      secret.password = v.password;
    }
  }
  return { connection, secret };
}

export interface ConnectionFormProps {
  value: FormValue;
  onChange: (v: FormValue) => void;
  shares: string[];
  testResult: TestResult | null;
  busy: boolean;
  /** Editing a connection whose password or key is already saved. */
  hasSavedSecret: boolean;
  onTest: () => void;
  onSave: () => void;
  onCancel: () => void;
  onAcceptHostKey: (fingerprint: string) => void;
  onPickKeyFile: () => void;
  onListShares: () => void;
}

export function ConnectionFormView(p: ConnectionFormProps) {
  const tr = useTr();
  const v = p.value;
  const set = (patch: Partial<FormValue>) => p.onChange({ ...v, ...patch });
  const failed = p.testResult && !p.testResult.ok ? p.testResult : null;
  const guestLabel =
    v.protocol === "smb"
      ? tr("conn_guest", undefined, "Guest (no user or password)")
      : tr("conn_anonymous", undefined, "Anonymous (no user or password)");
  const canTest = !!v.name.trim() && !!v.host.trim() && (v.protocol !== "smb" || !!v.share.trim());
  return (
    <div className="space-y-4">
      <SegmentedControl
        ariaLabel={tr("conn_protocol", undefined, "Protocol")}
        segments={[
          { value: "smb", label: "SMB" },
          { value: "ftp", label: "FTP" },
          { value: "ftps", label: "FTPS" },
          { value: "sftp", label: "SFTP" },
        ]}
        value={v.protocol}
        onChange={(pr) => p.onChange(switchProtocol(v, pr as Protocol))}
      />
      {v.protocol === "ftp" && (
        <p className="flex items-start gap-2 text-xs text-[var(--color-warn)]">
          <ShieldAlert size={14} className="mt-0.5 shrink-0" />
          {tr(
            "conn_ftp_unencrypted",
            undefined,
            "Unencrypted — use FTPS or SFTP where the server supports it.",
          )}
        </p>
      )}
      <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
        <Input
          label={tr("conn_name", undefined, "Name")}
          value={v.name}
          placeholder="NAS"
          onChange={(e) => set({ name: e.target.value })}
        />
        <div className="grid grid-cols-[1fr_6rem] gap-2">
          <Input
            label={tr("conn_host", undefined, "Server address")}
            value={v.host}
            placeholder="192.168.1.20"
            onChange={(e) => set({ host: e.target.value })}
          />
          <Input
            label={tr("conn_port", undefined, "Port")}
            type="number"
            inputMode="numeric"
            min={1}
            max={65535}
            value={v.port}
            onChange={(e) => set({ port: parseInt(e.target.value, 10) || DEFAULT_PORT[v.protocol] })}
          />
        </div>
        {v.protocol === "smb" && (
          <div>
            <Input
              label={tr("conn_share", undefined, "Share")}
              value={v.share}
              list="conn-shares"
              placeholder="games"
              onChange={(e) => set({ share: e.target.value })}
              rightSlot={
                <button
                  type="button"
                  className="text-xs text-[var(--color-accent)] hover:underline"
                  onClick={p.onListShares}
                >
                  {tr("conn_list_shares", undefined, "List shares")}
                </button>
              }
            />
            <datalist id="conn-shares">
              {p.shares.map((s) => (
                <option key={s} value={s} />
              ))}
            </datalist>
          </div>
        )}
        <Input
          label={tr("conn_start_path", undefined, "Start folder (optional)")}
          value={v.startPath}
          placeholder="/games"
          onChange={(e) => set({ startPath: e.target.value })}
        />
      </div>

      {v.protocol !== "sftp" && (
        <Toggle checked={v.guest} onChange={(guest) => set({ guest })} label={guestLabel} />
      )}

      {!v.guest && (
        <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
          <Input
            label={tr("conn_user", undefined, "User")}
            value={v.user}
            autoComplete="off"
            onChange={(e) => set({ user: e.target.value })}
          />
          {v.protocol === "sftp" && (
            <SegmentedControl
              ariaLabel={tr("conn_auth", undefined, "Sign in with")}
              segments={[
                { value: "password", label: tr("conn_password", undefined, "Password") },
                { value: "key", label: tr("conn_key_file", undefined, "Key file") },
              ]}
              value={v.authKind}
              onChange={(k) => set({ authKind: k as FormValue["authKind"] })}
            />
          )}
          {v.protocol === "sftp" && v.authKind === "key" ? (
            <div className="space-y-2">
              <Button variant="secondary" size="sm" onClick={p.onPickKeyFile}>
                <KeyRound size={14} />
                {v.keyName || tr("conn_choose_key", undefined, "Choose a key file…")}
              </Button>
              <Input
                label={tr("conn_key_passphrase", undefined, "Key passphrase (optional)")}
                type="password"
                autoComplete="off"
                value={v.keyPassphrase}
                onChange={(e) => set({ keyPassphrase: e.target.value })}
              />
            </div>
          ) : (
            <Input
              label={tr("conn_password", undefined, "Password")}
              type="password"
              autoComplete="off"
              value={v.password}
              placeholder={p.hasSavedSecret ? tr("conn_password_kept", undefined, "Saved — leave empty to keep it") : ""}
              onChange={(e) => set({ password: e.target.value })}
            />
          )}
        </div>
      )}

      {p.testResult?.ok && (
        <p className="flex items-center gap-2 text-sm text-[var(--color-good)]">
          <CheckCircle2 size={16} />
          {tr("conn_test_ok", undefined, "Connected.")}
        </p>
      )}
      {failed && (
        <div className="space-y-1 text-sm">
          <p className="flex items-start gap-2 text-[var(--color-bad)]">
            <XCircle size={16} className="mt-0.5 shrink-0" />
            {failed.error}
          </p>
          {failed.hint && <p className="text-xs text-[var(--color-muted)]">{failed.hint}</p>}
          {failed.host_key && (
            <div className="flex flex-wrap items-center gap-2 rounded-md border border-[var(--color-border)] p-2 text-xs">
              <AlertTriangle size={14} className="text-[var(--color-warn)]" />
              <span className="min-w-0 flex-1">
                {/changed/i.test(failed.error)
                  ? tr("conn_host_key_changed", undefined, "This server's key has changed since you last connected:")
                  : tr("conn_host_key_new", undefined, "First connection to this server. Its key fingerprint is:")}{" "}
                <code className="break-all font-mono">{failed.host_key}</code>
              </span>
              <Button variant="secondary" size="sm" onClick={() => p.onAcceptHostKey(failed.host_key!)}>
                {tr("conn_accept_key", undefined, "Accept")}
              </Button>
            </div>
          )}
        </div>
      )}

      <div className="flex flex-wrap justify-end gap-2">
        <Button variant="ghost" onClick={p.onCancel}>
          {tr("cancel", undefined, "Cancel")}
        </Button>
        <Button variant="secondary" disabled={!canTest || p.busy} loading={p.busy} onClick={p.onTest}>
          {tr("conn_test", undefined, "Test")}
        </Button>
        <Button variant="primary" disabled={!p.testResult?.ok || p.busy} onClick={p.onSave}>
          {tr("conn_save", undefined, "Save")}
        </Button>
      </div>
    </div>
  );
}
