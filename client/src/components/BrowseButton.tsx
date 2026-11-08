// "Browse…" for a source: the main click opens the system dialog (or the in-app browser where
// there is none); the ▾ beside it lists the saved servers, so a game on a NAS is one pick away.

import { useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { ChevronDown, FolderOpen, HardDrive, Plus, Server } from "lucide-react";

import type { Connection } from "../api/remote";
import { pickPath, pickPaths, type PickPathOptions } from "../lib/pickPath";
import { isMobile } from "../lib/platform";
import { displayPath, isRemotePath } from "../lib/remotePath";
import { useConnectionsStore, type ConnStatus } from "../state/connections";
import { getEngineUrl } from "../state/engine";
import { Spinner } from "./Spinner";
import { useTr } from "../state/lang";
import { useFitOnScreen } from "./useFitOnScreen";

const PROTOCOL: Record<Connection["protocol"], string> = {
  smb: "SMB",
  ftp: "FTP",
  ftps: "FTPS",
  sftp: "SFTP",
  webdav: "WebDAV",
  webdavs: "WebDAV (HTTPS)",
  nfs: "NFS",
};

export interface BrowseButtonProps {
  mode: "file" | "folder";
  title?: string;
  filters?: PickPathOptions["filters"];
  /** This screen can read from a saved server. */
  remote?: boolean;
  label?: string;
  disabled?: boolean;
  /** The screen's main action: filled in the accent color, like a primary Button. */
  primary?: boolean;
  /** Spinner on the main button while its work runs. */
  busy?: boolean;
  /** Hover text on the main button. */
  tooltip?: string;
  icon?: React.ReactNode;
  className?: string;
  /** Replaces the main click (a screen with its own local flow keeps it); the ▾ still picks
   *  from servers and hands the path to `onPick`. */
  onMainClick?: () => void;
  onPick: (path: string) => void;
  /** Files only: pick several at once (ticked rows in the in-app browser, the system dialog's
   *  multi-select) and hand them here instead of to `onPick`. */
  onPickMany?: (paths: string[]) => void;
  /** Web UI: also offer the device the browser runs on. The main click and "On the server…"
   *  keep reading the engine's computer; this runs for "This device…" (#730). */
  thisDevice?: () => void;
}

/** The engine's computer, by the name the address bar knows it by. */
export function engineHostName(): string {
  try {
    const base = typeof window === "undefined" ? "http://localhost" : window.location.href;
    return new URL(getEngineUrl() || base, base).hostname;
  } catch {
    return "";
  }
}

export function BrowseMenu(props: {
  connections: Connection[];
  status: Record<string, ConnStatus>;
  onLocal: () => void;
  onPickServer: (id: string) => void;
  onAdd: () => void;
  /** Web UI: "On the server…" and "This device…", each saying which machine it reads. */
  onThisDevice?: () => void;
  /** List the saved servers (and Add a connection). */
  servers?: boolean;
}) {
  const tr = useTr();
  const ref = useRef<HTMLDivElement>(null);
  const fit = useFitOnScreen(ref, true);
  const item =
    "flex w-full items-center gap-2 px-3 py-2 text-start text-sm hover:bg-[var(--color-surface-3)] disabled:opacity-50";
  const servers = props.servers ?? true;
  const host = props.onThisDevice ? engineHostName() : "";
  return (
    <div
      ref={ref}
      role="menu"
      className="glass-float elev-3 absolute end-0 top-full z-40 mt-1 min-w-56 max-w-[calc(100vw-1rem)] overflow-y-auto rounded-[var(--radius-card)] p-1.5"
      style={fit}
    >
      {props.onThisDevice ? (
        <>
          <button
            type="button"
            role="menuitem"
            className={`${item} min-h-11 items-start`}
            onClick={props.onLocal}
            data-testid="browse-on-server"
          >
            <Server size={14} className="mt-0.5 shrink-0 text-[var(--color-muted)]" />
            <span className="w-64 max-w-[70vw]">
              <span className="block font-medium">{tr("browse_on_server", undefined, "On the server…")}</span>
              <span className="mt-0.5 block text-xs leading-snug text-[var(--color-muted)]">
                {tr(
                  "browse_on_server_hint",
                  { host },
                  `Reads from ${host}, the computer the engine runs on. The quickest: it goes straight to the PS5.`,
                )}
              </span>
            </span>
          </button>
          <button
            type="button"
            role="menuitem"
            className={`${item} min-h-11 items-start`}
            onClick={props.onThisDevice}
            data-testid="browse-this-device"
          >
            <HardDrive size={14} className="mt-0.5 shrink-0 text-[var(--color-muted)]" />
            <span className="w-64 max-w-[70vw]">
              <span className="block font-medium">{tr("browse_this_device", undefined, "This device…")}</span>
              <span className="mt-0.5 block text-xs leading-snug text-[var(--color-muted)]">
                {tr(
                  "browse_this_device_hint",
                  undefined,
                  "Reads from the device this browser runs on. It is copied twice, to the server and then to the PS5: slower, and the server needs free space for the copy.",
                )}
              </span>
            </span>
          </button>
        </>
      ) : (
        <button type="button" role="menuitem" className={item} onClick={props.onLocal}>
          <HardDrive size={14} className="text-[var(--color-muted)]" />
          {isMobile()
            ? tr("browse_this_device", undefined, "This device…")
            : tr("browse_this_computer", undefined, "This computer…")}
        </button>
      )}
      {servers && props.connections.length > 0 && <div className="my-1 border-t border-[var(--color-border)]" />}
      {servers && props.connections.map((c) => {
        const offline = props.status[c.id] === "offline";
        return (
          <button
            key={c.id}
            type="button"
            role="menuitem"
            className={item}
            onClick={() => props.onPickServer(c.id)}
          >
            <Server size={14} className={offline ? "text-[var(--color-muted)]" : "text-[var(--color-accent)]"} />
            <span className={`truncate ${offline ? "text-[var(--color-muted)]" : ""}`}>{c.name}</span>
            <span className="text-xs text-[var(--color-muted)]">({PROTOCOL[c.protocol]})</span>
            {offline && (
              <span className="ms-auto text-xs text-[var(--color-muted)]">
                {tr("browse_not_reachable", undefined, "not reachable")}
              </span>
            )}
          </button>
        );
      })}
      {servers && (
        <>
          <div className="my-1 border-t border-[var(--color-border)]" />
          <button type="button" role="menuitem" className={item} onClick={props.onAdd}>
            <Plus size={14} className="text-[var(--color-muted)]" />
            {tr("browse_add_connection", undefined, "Add a connection…")}
          </button>
        </>
      )}
    </div>
  );
}

export function BrowseButton(props: BrowseButtonProps) {
  const tr = useTr();
  const navigate = useNavigate();
  const [open, setOpen] = useState(false);
  const wrap = useRef<HTMLDivElement>(null);
  const connections = useConnectionsStore((s) => s.connections);
  const loaded = useConnectionsStore((s) => s.loaded);
  const status = useConnectionsStore((s) => s.status);
  const load = useConnectionsStore((s) => s.load);

  useEffect(() => {
    if (open && !loaded) void load().catch(() => {});
  }, [open, loaded, load]);

  // Close on a click outside or Escape.
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (wrap.current && !wrap.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    window.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const pick = async (source?: { connectionId: string }) => {
    setOpen(false);
    if (props.onPickMany && props.mode === "file") {
      const paths = await pickPaths({
        mode: "file",
        title: props.title,
        filters: props.filters,
        source,
      });
      if (paths.length) props.onPickMany(paths);
      return;
    }
    const path = await pickPath({
      mode: props.mode,
      title: props.title,
      filters: props.filters,
      source,
    });
    if (path) props.onPick(path);
  };

  const tone = props.primary
    ? "border-transparent bg-[var(--color-accent)] bg-[image:var(--accent-fill)] text-[var(--color-accent-contrast)] shadow-[var(--accent-glow)] hover:brightness-105"
    : "border-[var(--glass-edge)] bg-[var(--color-surface-raised)] shadow-[var(--edge-highlight),var(--shadow-1)] hover:bg-[var(--color-float)]";
  const base = `inline-flex min-h-9 items-center gap-1.5 border ${tone} px-4 text-sm font-medium disabled:opacity-50`;
  const label = props.label ?? tr("browse", undefined, "Browse…");
  const split = !!props.remote || !!props.thisDevice;
  return (
    <div ref={wrap} className={`relative inline-flex ${props.className ?? ""}`}>
      <button
        type="button"
        disabled={props.disabled || props.busy}
        title={props.tooltip}
        className={`${base} ${split ? "rounded-s-full" : "rounded-full"}`}
        onClick={() => (props.onMainClick ? props.onMainClick() : void pick())}
      >
        {props.busy ? <Spinner size={14} tone="inherit" /> : (props.icon ?? <FolderOpen size={14} />)}
        {label}
      </button>
      {split && (
        <button
          type="button"
          disabled={props.disabled}
          aria-haspopup="menu"
          aria-expanded={open}
          aria-label={
            props.thisDevice
              ? tr("browse_choose_source", undefined, "Choose where to read from")
              : tr("browse_servers", undefined, "Browse a server")
          }
          data-testid="browse-source-menu"
          className={`${base} rounded-e-full px-2.5 ${
            props.primary ? "border-s-[var(--color-accent-contrast)]/30" : "border-s-0"
          }`}
          onClick={() => setOpen((v) => !v)}
          onKeyDown={(e) => e.key === "ArrowDown" && setOpen(true)}
        >
          <ChevronDown size={14} />
        </button>
      )}
      {open && (
        <BrowseMenu
          connections={connections}
          status={status}
          onLocal={() => {
            setOpen(false);
            if (props.onMainClick) props.onMainClick();
            else void pick();
          }}
          onPickServer={(id) => void pick({ connectionId: id })}
          onAdd={() => {
            setOpen(false);
            navigate("/shared-folders?add=1");
          }}
          servers={!!props.remote}
          onThisDevice={
            props.thisDevice
              ? () => {
                  setOpen(false);
                  props.thisDevice?.();
                }
              : undefined
          }
        />
      )}
    </div>
  );
}

/** A picked path as a person reads it: a server's by the server's name, a local one as is. */
export function PathLabelView(props: {
  path: string;
  nameOf: (id: string) => string | undefined;
  className?: string;
}) {
  const remote = isRemotePath(props.path);
  const text = displayPath(props.path, props.nameOf);
  return (
    <span className={`inline-flex min-w-0 items-center gap-1 ${props.className ?? ""}`} title={text}>
      {remote && <Server size={12} className="shrink-0 text-[var(--color-accent)]" />}
      <span className="truncate">{text}</span>
    </span>
  );
}

export function PathLabel({ path, className }: { path: string; className?: string }) {
  const connections = useConnectionsStore((s) => s.connections);
  return (
    <PathLabelView
      path={path}
      className={className}
      nameOf={(id) => connections.find((c) => c.id === id)?.name}
    />
  );
}
