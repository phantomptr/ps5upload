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
  /** SFTP/FTP: browse the whole server (paths from its "/"), not just the sign-in folder. */
  fromRoot: boolean;
  /** WebDAV over HTTPS: accept a self-signed certificate. */
  insecureTls: boolean;
}

export const DEFAULT_PORT: Record<Protocol, number> = {
  smb: 445,
  ftp: 21,
  ftps: 21,
  sftp: 22,
  webdav: 80,
  webdavs: 443,
  nfs: 2049,
};

/** The protocols whose paths can start at the server's "/" or at the sign-in folder. */
export function hasServerRoot(p: Protocol): boolean {
  return p === "sftp" || p === "ftp" || p === "ftps";
}

/** Both WebDAV flavours. */
export function isWebdav(p: Protocol): boolean {
  return p === "webdav" || p === "webdavs";
}

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
    fromRoot: true,
    insecureTls: false,
  };
}

/** A new protocol brings its default port, unless the user had typed their own. */
export function switchProtocol(v: FormValue, protocol: Protocol): FormValue {
  const port = v.port === DEFAULT_PORT[v.protocol] ? DEFAULT_PORT[protocol] : v.port;
  const next = { ...v, protocol, port, authKind: protocol === "sftp" ? v.authKind : ("password" as const) };
  // NFS's user is a user and group ID and its share an export: neither carries over.
  return (protocol === "nfs") !== (v.protocol === "nfs")
    ? { ...next, user: "", share: "", password: "", guest: false }
    : next;
}

const SMB_SCHEME = /^(smb|cifs):\/\//i;

function pathSegs(s: string): string[] {
  return s.split(/[/\\]+/).map((x) => x.trim()).filter(Boolean);
}

/**
 * SMB: a whole address pasted into Server or Share (`\\\\nas\\Media\\Games`,
 * `smb://nas/Media/Games`) or a share typed with folders after it (`Media/Games/PS5`) is split
 * the way the server needs it: the server alone in Server, the share's own name in Share (the
 * first folder name only), and the folders inside it in front of Start folder. A port typed
 * after the server (`nas:4455`) moves to Port.
 */
export function tidySmbAddress(v: FormValue): FormValue {
  if (v.protocol !== "smb") return v;
  const hostSegs = pathSegs(v.host.trim().replace(SMB_SCHEME, ""));
  let host = hostSegs[0] ?? "";
  let shareSegs = pathSegs(v.share.trim().replace(SMB_SCHEME, ""));
  const shareIsAddress = SMB_SCHEME.test(v.share.trim()) || /^(\\\\|\/\/)/.test(v.share.trim());
  if (shareIsAddress && shareSegs.length >= 2) {
    const server = shareSegs.shift()!;
    if (!host) host = server;
  }
  if (shareSegs.length === 0) shareSegs = hostSegs.slice(1);
  let port = v.port;
  const hp = /^([^:[\]]+):(\d{1,5})$/.exec(host);
  if (hp && Number(hp[2]) > 0 && Number(hp[2]) <= 65535) {
    host = hp[1];
    port = Number(hp[2]);
  }
  const share = shareSegs[0] ?? "";
  const inner = shareSegs.slice(1);
  const startPath = inner.length ? `/${[...inner, ...pathSegs(v.startPath)].join("/")}` : v.startPath;
  if (host === v.host && share === v.share && startPath === v.startPath && port === v.port) return v;
  // Nothing recognisable typed yet: leave the fields as they are.
  if (!host) return v;
  return { ...v, host, share, startPath, port };
}

/** The form as the engine takes it. An empty secret on an edit keeps the saved one. */
export function toRequest(form: FormValue): { connection: ConnectionInput; secret: SecretInput } {
  const v = tidySmbAddress(form);
  const connection: ConnectionInput = {
    name: v.name.trim(),
    protocol: v.protocol,
    host: v.host.trim(),
    port: v.port,
    share: v.protocol === "smb" || v.protocol === "nfs" ? v.share.trim() : "",
    user: v.guest && v.protocol !== "nfs" ? "" : v.user.trim(),
    start_path: v.startPath.trim(),
    from_root: hasServerRoot(v.protocol) && v.fromRoot,
    insecure_tls: v.protocol === "webdavs" && v.insecureTls,
  };
  const secret: SecretInput = {};
  if (!v.guest && v.protocol !== "nfs") {
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
  /** Editing: whether the saved connection browses the whole server (null for a new one). */
  savedFromRoot?: boolean | null;
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
      : isWebdav(v.protocol)
        ? tr("conn_no_signin", undefined, "No sign-in (public share)")
        : tr("conn_anonymous", undefined, "Anonymous (no user or password)");
  const tidy = tidySmbAddress(v);
  const canTest = !!v.name.trim() && !!tidy.host.trim() && (v.protocol !== "smb" || !!tidy.share.trim());
  // Tidied when the field is left, so the user sees where each part went.
  const tidyOnBlur = () => {
    if (tidy !== v) p.onChange(tidy);
  };
  // The server has no share by that name: said in the user's language, naming the share tried.
  const noShare =
    failed && v.protocol === "smb" && /bad_network_name|no share named/i.test(failed.error)
      ? (/No share named '([^']*)'/.exec(failed.error)?.[1] ?? tidy.share) || "?"
      : null;
  return (
    <div className="space-y-4">
      <SegmentedControl
        ariaLabel={tr("conn_protocol", undefined, "Protocol")}
        segments={[
          { value: "smb", label: "SMB" },
          { value: "ftp", label: "FTP" },
          { value: "ftps", label: "FTPS" },
          { value: "sftp", label: "SFTP" },
          { value: "webdav", label: "WebDAV" },
          { value: "webdavs", label: "WebDAV (HTTPS)" },
          { value: "nfs", label: "NFS" },
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
      {v.protocol === "webdav" && (
        <p className="flex items-start gap-2 text-xs text-[var(--color-warn)]">
          <ShieldAlert size={14} className="mt-0.5 shrink-0" />
          {tr(
            "conn_webdav_unencrypted",
            undefined,
            "Unencrypted — use WebDAV (HTTPS) where the server supports it.",
          )}
        </p>
      )}
      {isWebdav(v.protocol) && (
        <p className="text-xs text-[var(--color-muted)]">
          {tr(
            "conn_webdav_root_hint",
            undefined,
            "Put the server's WebDAV root in Start folder: /remote.php/dav/files/USER for Nextcloud, /dav for Alist, / for rclone serve webdav. Sign-in is Basic authentication.",
          )}
        </p>
      )}
      <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
        <Input
          label={tr("conn_name", undefined, "Name")}
          value={v.name}
          placeholder={tr("conn_name_placeholder", undefined, "NAS")}
          onChange={(e) => set({ name: e.target.value })}
        />
        <div className="grid grid-cols-[1fr_6rem] gap-2">
          <Input
            label={tr("conn_host", undefined, "Server address")}
            value={v.host}
            placeholder="192.168.1.20"
            onChange={(e) => set({ host: e.target.value })}
            onBlur={tidyOnBlur}
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
        {v.protocol === "nfs" && (
          <Input
            label={tr("conn_nfs_export", undefined, "Export (empty: all of them)")}
            value={v.share}
            placeholder="/volume1/games"
            onChange={(e) => set({ share: e.target.value })}
          />
        )}
        {v.protocol === "smb" && (
          <div>
            <Input
              label={tr("conn_share", undefined, "Share")}
              value={v.share}
              list="conn-shares"
              placeholder={tr("conn_share_placeholder", undefined, "games")}
              onChange={(e) => set({ share: e.target.value })}
              onBlur={tidyOnBlur}
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
          placeholder={
            isWebdav(v.protocol)
              ? "/remote.php/dav/files/USER"
              : hasServerRoot(v.protocol) && v.fromRoot
                ? "/mnt/games"
                : "/games"
          }
          onChange={(e) => set({ startPath: e.target.value })}
          data-testid="conn-start-path"
        />
      </div>

      {hasServerRoot(v.protocol) && (
        <div className="space-y-1" data-testid="conn-from-root">
          <Toggle
            checked={v.fromRoot}
            onChange={(fromRoot) => set({ fromRoot })}
            label={tr("conn_from_root", undefined, "Browse the whole server")}
            hint={
              v.fromRoot
                ? tr(
                    "conn_from_root_on",
                    undefined,
                    "Folders are the server's own paths, as an SSH or FTP client shows them (/home/you, /mnt/games): the start folder and browsing can be anywhere this account may read. With no start folder, it opens where the server signs you in.",
                  )
                : tr(
                    "conn_from_root_off",
                    undefined,
                    "Only the folder the server signs you in to and what is inside it: / is that folder, and the start folder is inside it.",
                  )
            }
          />
          {p.savedFromRoot === false && v.fromRoot && (
            <p className="flex items-start gap-1.5 text-xs text-[var(--color-warn)]" data-testid="conn-from-root-relink">
              <AlertTriangle size={14} className="mt-0.5 shrink-0" />
              {tr(
                "conn_from_root_relink",
                undefined,
                "Games, favorites and links already saved from this server use the old paths: add them again after this change.",
              )}
            </p>
          )}
        </div>
      )}

      {v.protocol === "webdavs" && (
        <Toggle
          checked={v.insecureTls}
          onChange={(insecureTls) => set({ insecureTls })}
          label={tr("conn_insecure_tls", undefined, "Accept a self-signed certificate")}
          hint={tr(
            "conn_insecure_tls_hint",
            undefined,
            "Only for a NAS on your own network whose certificate is self-signed. Anyone between this computer and the server could then read the connection.",
          )}
        />
      )}

      {v.protocol !== "sftp" && v.protocol !== "nfs" && (
        <Toggle checked={v.guest} onChange={(guest) => set({ guest })} label={guestLabel} />
      )}

      {v.protocol === "nfs" && (
        <Input
          label={tr("conn_nfs_ids", undefined, "User and group ID")}
          value={v.user}
          placeholder="1000:1000"
          autoComplete="off"
          hint={tr(
            "conn_nfs_ids_hint",
            undefined,
            "NFS has no passwords: the server trusts these IDs, and the export decides who may connect. Exports marked \"secure\" also need a port only an administrator can open, so mark yours \"insecure\" if it refuses this computer.",
          )}
          onChange={(e) => set({ user: e.target.value })}
        />
      )}

      {!v.guest && v.protocol !== "nfs" && (
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
            {noShare
              ? tr(
                  "conn_smb_no_share",
                  { share: noShare },
                  `No share named '${noShare}' on this server. The share is the first folder name only (for example Media in \\\\nas\\Media\\Games); folders inside it go in Start folder. List shares shows the names this server offers.`,
                )
              : failed.error}
          </p>
          {failed.hint && !noShare && <p className="text-xs text-[var(--color-muted)]">{failed.hint}</p>}
          {failed.host_key && (
            <div className="flex flex-wrap items-center gap-2 rounded-[var(--radius-card)] border border-[var(--glass-edge)] p-2 text-xs">
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
