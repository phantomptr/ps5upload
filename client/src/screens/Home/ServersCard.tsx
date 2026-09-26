// Home's view of the saved servers: one tap to browse a NAS, or to connect the first one.

import { useEffect } from "react";
import { useNavigate } from "react-router";
import { FolderOpen, Network, Plus } from "lucide-react";

import { Button, Card } from "../../components";
import type { Connection } from "../../api/remote";
import { useConnectionsStore, type ConnStatus } from "../../state/connections";
import { browseServer } from "../../lib/browseServer";
import { useTr } from "../../state/lang";

const PROTOCOL: Record<Connection["protocol"], string> = {
  smb: "SMB",
  ftp: "FTP",
  ftps: "FTPS",
  sftp: "SFTP",
};

export function ServersCardView(props: {
  connections: Connection[];
  status: Record<string, ConnStatus>;
  onBrowse: (c: Connection) => void;
  onAdd: () => void;
}) {
  const tr = useTr();
  return (
    <Card className="xl:col-span-12">
      <div className="mb-3 flex items-center gap-2">
        <Network size={16} className="text-[var(--color-muted)]" />
        <h2 className="flex-1 text-sm font-semibold">{tr("v5_home_servers", undefined, "Servers")}</h2>
        {props.connections.length > 0 && (
          <Button variant="ghost" size="sm" onClick={props.onAdd}>
            <Plus size={14} />
            {tr("conn_add", undefined, "Add a server")}
          </Button>
        )}
      </div>
      {props.connections.length === 0 ? (
        <div className="flex flex-wrap items-center gap-3">
          <p className="flex-1 text-xs text-[var(--color-muted)]">
            {tr(
              "v5_home_servers_empty",
              undefined,
              "Pick games straight from your NAS or a shared folder, over SMB, FTP or SFTP.",
            )}
          </p>
          <Button variant="primary" size="sm" onClick={props.onAdd}>
            <Plus size={14} />
            {tr("v5_home_servers_connect", undefined, "Connect a NAS or server")}
          </Button>
        </div>
      ) : (
        <ul className="flex flex-wrap gap-2">
          {props.connections.map((c) => (
            <li
              key={c.id}
              className="flex min-w-48 items-center gap-2 rounded-lg border border-[var(--color-border)] px-3 py-2"
            >
              <span
                className={`h-2 w-2 shrink-0 rounded-full ${
                  props.status[c.id] === "reachable"
                    ? "bg-[var(--color-good)]"
                    : props.status[c.id] === "auth-failed"
                      ? "bg-[var(--color-bad)]"
                      : "bg-[var(--color-muted)]"
                }`}
                aria-hidden
              />
              <span className="min-w-0 flex-1 truncate text-sm font-medium">{c.name}</span>
              <span className="text-xs text-[var(--color-muted)]">{PROTOCOL[c.protocol]}</span>
              <Button variant="ghost" size="sm" onClick={() => props.onBrowse(c)}>
                <FolderOpen size={14} />
                {tr("conn_browse", undefined, "Browse")}
              </Button>
            </li>
          ))}
        </ul>
      )}
    </Card>
  );
}

export function ServersCard() {
  const tr = useTr();
  const navigate = useNavigate();
  const connections = useConnectionsStore((s) => s.connections);
  const loaded = useConnectionsStore((s) => s.loaded);
  const status = useConnectionsStore((s) => s.status);
  const load = useConnectionsStore((s) => s.load);

  useEffect(() => {
    if (!loaded) void load().catch(() => {});
  }, [loaded, load]);

  return (
    <ServersCardView
      connections={connections}
      status={status}
      onAdd={() => navigate("/connections?add=1")}
      onBrowse={(c) =>
        browseServer(c, navigate, tr("conn_need_ps5", undefined, "Connect to a PS5 to install."))
      }
    />
  );
}
