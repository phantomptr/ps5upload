// "Browse…" for a source: the main click opens the system dialog (or the in-app browser where
// there is none); the ▾ beside it lists the saved servers, so a game on a NAS is one pick away.

import { useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { ChevronDown, FolderOpen, HardDrive, Plus, Server } from "lucide-react";

import type { Connection } from "../api/remote";
import { pickPath, type PickPathOptions } from "../lib/pickPath";
import { displayPath, isRemotePath } from "../lib/remotePath";
import { useConnectionsStore, type ConnStatus } from "../state/connections";
import { useTr } from "../state/lang";

const PROTOCOL: Record<Connection["protocol"], string> = {
  smb: "SMB",
  ftp: "FTP",
  ftps: "FTPS",
  sftp: "SFTP",
};

export interface BrowseButtonProps {
  mode: "file" | "folder";
  title?: string;
  filters?: PickPathOptions["filters"];
  /** This screen can read from a saved server. */
  remote?: boolean;
  label?: string;
  disabled?: boolean;
  className?: string;
  onPick: (path: string) => void;
}

export function BrowseMenu(props: {
  connections: Connection[];
  status: Record<string, ConnStatus>;
  onLocal: () => void;
  onPickServer: (id: string) => void;
  onAdd: () => void;
}) {
  const tr = useTr();
  const item =
    "flex w-full items-center gap-2 px-3 py-2 text-left text-sm hover:bg-[var(--color-surface-3)] disabled:opacity-50";
  return (
    <div
      role="menu"
      className="elev-3 absolute right-0 z-40 mt-1 min-w-56 overflow-hidden rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] py-1"
    >
      <button type="button" role="menuitem" className={item} onClick={props.onLocal}>
        <HardDrive size={14} className="text-[var(--color-muted)]" />
        {tr("browse_this_computer", undefined, "This computer…")}
      </button>
      {props.connections.length > 0 && <div className="my-1 border-t border-[var(--color-border)]" />}
      {props.connections.map((c) => {
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
      <div className="my-1 border-t border-[var(--color-border)]" />
      <button type="button" role="menuitem" className={item} onClick={props.onAdd}>
        <Plus size={14} className="text-[var(--color-muted)]" />
        {tr("browse_add_connection", undefined, "Add a connection…")}
      </button>
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
    const path = await pickPath({
      mode: props.mode,
      title: props.title,
      filters: props.filters,
      source,
    });
    if (path) props.onPick(path);
  };

  const base =
    "inline-flex min-h-8 items-center gap-1.5 border border-[var(--color-border)] bg-[var(--color-surface-2)] px-3 text-sm hover:bg-[var(--color-surface-3)] disabled:opacity-50";
  const label = props.label ?? tr("browse", undefined, "Browse…");
  return (
    <div ref={wrap} className={`relative inline-flex ${props.className ?? ""}`}>
      <button
        type="button"
        disabled={props.disabled}
        className={`${base} ${props.remote ? "rounded-s-md" : "rounded-md"}`}
        onClick={() => void pick()}
      >
        <FolderOpen size={14} />
        {label}
      </button>
      {props.remote && (
        <button
          type="button"
          disabled={props.disabled}
          aria-haspopup="menu"
          aria-expanded={open}
          aria-label={tr("browse_servers", undefined, "Browse a server")}
          className={`${base} rounded-e-md border-s-0 px-2`}
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
          onLocal={() => void pick()}
          onPickServer={(id) => void pick({ connectionId: id })}
          onAdd={() => {
            setOpen(false);
            navigate("/connections?add=1");
          }}
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
