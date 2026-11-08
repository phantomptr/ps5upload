// Save to: a server. The saved server (Connections) and the folder on it an image or a game
// folder is written to, straight, with nothing stored on this computer. The folder is kept as a
// `remote://<connection>/<folder>` path, as the server browser hands it back.

import { useEffect, useId } from "react";
import { FolderOpen } from "lucide-react";

import { Button } from "../../components";
import { pickPath } from "../../lib/pickPath";
import { displayPath, parseRemotePath, remotePath } from "../../lib/remotePath";
import { useConnectionsStore } from "../../state/connections";
import { useTr } from "../../state/lang";

export function ServerDestination({
  folder,
  onChange,
}: {
  /** The folder chosen, as a `remote://` path; "" when none is chosen yet. */
  folder: string;
  onChange: (folder: string) => void;
}) {
  const tr = useTr();
  const serverId = useId();
  const connections = useConnectionsStore((s) => s.connections);
  const loaded = useConnectionsStore((s) => s.loaded);
  const load = useConnectionsStore((s) => s.load);
  useEffect(() => {
    if (!loaded) void load().catch(() => {});
  }, [loaded, load]);

  const chosen = parseRemotePath(folder);
  // A folder on a server that has since been removed is not offered.
  const known = chosen && connections.some((c) => c.id === chosen.connectionId) ? chosen : null;
  const server = known?.connectionId ?? "";
  const nameOf = (id: string) => connections.find((c) => c.id === id)?.name;

  async function chooseFolder(id: string) {
    if (!id) return;
    const picked = await pickPath({
      mode: "folder",
      source: { connectionId: id },
      title: tr("fpkg.save_to_server_pick", undefined, "Folder on the server to save it in"),
    });
    if (picked) onChange(picked);
  }

  if (loaded && connections.length === 0) {
    return (
      <span className="text-xs text-[var(--color-warn)]" data-testid="server-dest-none">
        {tr("fpkg.save_to_server_none", undefined, "Add a server under Connections first.")}
      </span>
    );
  }
  return (
    <div className="flex flex-col gap-2 text-sm" data-testid="server-dest">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <label htmlFor={serverId} className="text-[var(--color-muted)]">
          {tr("saves_server_server", undefined, "Server")}
        </label>
        <select
          id={serverId}
          className="select w-auto!"
          data-testid="server-dest-server"
          value={server}
          onChange={(e) => {
            const id = e.target.value;
            // Another server starts at its top folder; Choose folder goes deeper.
            onChange(id ? remotePath(id, "/") : "");
          }}
        >
          <option value="" disabled>
            {tr("fpkg.save_to_server_choose", undefined, "Choose a server")}
          </option>
          {connections.map((c) => (
            <option key={c.id} value={c.id}>
              {c.name}
            </option>
          ))}
        </select>
        <Button
          variant="secondary"
          size="sm"
          leftIcon={<FolderOpen size={12} />}
          disabled={!server}
          onClick={() => void chooseFolder(server)}
          data-testid="server-dest-browse"
        >
          {tr("saves_auto_folder", undefined, "Choose folder")}
        </Button>
      </div>
      <span className="break-all text-xs text-[var(--color-muted)]" data-testid="server-dest-path">
        {known
          ? (() => {
              const path = `${displayPath(folder, nameOf)} › …`;
              return tr("image_dest_goes_to", { path }, `Goes to ${path}`);
            })()
          : tr("fpkg.save_to_server_no_folder", undefined, "Choose a server and a folder on it.")}
      </span>
    </div>
  );
}
