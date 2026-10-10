import { useEffect, useState } from "react";
import { FolderPlus, Server, Trash2 } from "lucide-react";

import { remoteApi, type Connection } from "../../api/remote";
import { pickPath } from "../../lib/pickPath";

import { Button, Modal, Select, Toggle } from "../../components";
import {
  addCollectionRoot,
  removeCollectionRoot,
  setCollectionPermanentDelete,
  setCollectionRefresh,
  useCollectionStore,
} from "../../state/collection";
import { useTr } from "../../state/lang";

/** Which folders the Collection scans, and how often it looks again on its own. */
export function FoldersModal({
  open,
  onClose,
  onAdd,
}: {
  open: boolean;
  onClose: () => void;
  onAdd: () => void;
}) {
  const tr = useTr();
  const [servers, setServers] = useState<Connection[]>([]);
  useEffect(() => {
    if (!open) return;
    remoteApi
      .list()
      .then(setServers)
      .catch(() => setServers([]));
  }, [open]);
  /** `remote://<id>/<path>` → "NAS · /games"; a local path as it is. */
  const shown = (r: string) => {
    const m = /^remote:\/\/([^/]+)(\/.*)?$/.exec(r);
    if (!m) return r;
    const name = servers.find((c) => c.id === m[1])?.name ?? m[1];
    return `${name} · ${m[2] ?? "/"}`;
  };
  const addServerFolder = async (c: Connection) => {
    const path = await pickPath({
      mode: "folder",
      title: tr(
        "collection.pick_server_folder",
        { name: c.name },
        "The folder on {name} that holds your games",
      ),
      source: { connectionId: c.id },
    });
    if (path) await addCollectionRoot(path);
  };
  const settings = useCollectionStore((s) => s.settings);
  const library = useCollectionStore((s) => s.library);
  const roots = settings?.roots ?? [];
  const choices = settings?.refresh_choices ?? [
    30, 60, 300, 900, 1800, 3600, 7200, 86400,
  ];
  const every = (secs: number) =>
    secs < 60
      ? tr("collection.every_s", { n: secs }, "Every {n} seconds")
      : secs < 3600
        ? tr("collection.every_m", { n: secs / 60 }, "Every {n} minutes")
        : secs < 86400
          ? tr("collection.every_h", { n: secs / 3600 }, "Every {n} hours")
          : tr("collection.every_day", undefined, "Once a day");
  return (
    <Modal
      open={open}
      onClose={onClose}
      size="lg"
      title={tr("collection.folders_title", undefined, "Collection folders")}
      bodyClassName="p-4 sm:p-5"
    >
      <p className="mb-3 text-xs text-[var(--color-muted)]">
        {tr(
          "collection.folders_body",
          undefined,
          "Everything under these folders is looked at, up to six folders deep. The folders can be arranged and named any way you like: a game is recognised by its files.",
        )}
      </p>
      <ul className="mb-3 flex flex-col gap-1.5">
        {roots.map((r) => (
          <li
            key={r}
            className="flex items-center gap-2 rounded-2xl border border-[var(--glass-edge)] bg-[var(--color-surface)] px-3 py-2"
          >
            {r.startsWith("remote://") && (
              <Server
                size={13}
                className="shrink-0 text-[var(--color-muted)]"
              />
            )}
            <span
              className="min-w-0 flex-1 break-all font-mono text-xs"
              title={r}
            >
              {shown(r)}
            </span>
            <button
              type="button"
              onClick={() => void removeCollectionRoot(r)}
              className="shrink-0 rounded p-1 text-[var(--color-muted)] hover:text-[var(--color-bad)]"
              title={tr(
                "collection.remove_folder",
                undefined,
                "Stop scanning this folder (nothing in it is changed)",
              )}
              aria-label={tr(
                "collection.remove_folder_short",
                undefined,
                "Remove folder",
              )}
            >
              <Trash2 size={14} />
            </button>
          </li>
        ))}
      </ul>
      <Button
        size="sm"
        variant="secondary"
        leftIcon={<FolderPlus size={14} />}
        onClick={onAdd}
      >
        {tr("collection.add_folder", undefined, "Add a folder")}
      </Button>
      <div className="mt-3 flex flex-wrap items-center gap-2">
        {servers.map((c) => (
          <Button
            key={c.id}
            size="sm"
            variant="secondary"
            leftIcon={<Server size={14} />}
            onClick={() => void addServerFolder(c)}
          >
            {tr(
              "collection.add_server_folder",
              { name: c.name },
              "Add a folder on {name}",
            )}
          </Button>
        ))}
      </div>
      <p className="mt-1 text-xs text-[var(--color-muted)]">
        {servers.length > 0
          ? tr(
              "collection.server_folders_hint",
              undefined,
              "A folder on a saved server (SMB, FTP, SFTP) is read in place: nothing is copied to this computer, and its games install straight from the server. Move to Trash and Organize work on this computer's folders only.",
            )
          : tr(
              "collection.no_servers_hint",
              undefined,
              "Games on a NAS? Save it under Connections (SMB, FTP or SFTP), and its folders can be added here without mounting them.",
            )}
      </p>
      <div className="mt-5">
        <Select
          label={tr(
            "collection.refresh",
            undefined,
            "Look for changes automatically",
          )}
          value={
            settings?.refresh_secs == null
              ? "off"
              : String(settings.refresh_secs)
          }
          onChange={(e) =>
            void setCollectionRefresh(
              e.target.value === "off" ? null : Number(e.target.value),
            )
          }
          options={[
            {
              value: "off",
              label: tr(
                "collection.refresh_off",
                undefined,
                "Off (scan only when I press Scan)",
              ),
            },
            ...choices.map((c) => ({ value: String(c), label: every(c) })),
          ]}
          hint={tr(
            "collection.refresh_hint",
            undefined,
            "Only while the app is open. It never keeps the computer awake, and after it wakes the wait starts over instead of scanning at once.",
          )}
        />
      </div>
      {settings?.trash_available === false && (
        <div className="mt-5">
          <Toggle
            checked={!!settings.allow_permanent_delete}
            onChange={(on) => void setCollectionPermanentDelete(on)}
            label={tr(
              "collection.permanent_delete",
              undefined,
              "Allow deleting copies for good",
            )}
            hint={tr(
              "collection.permanent_delete_hint",
              undefined,
              "This engine has no Trash (Docker, or a server without a desktop). With this on, a copy can be deleted from the Collection: it is gone for good, and you confirm every time.",
            )}
          />
        </div>
      )}
      {library?.generated_at && (
        <p className="mt-3 text-xs text-[var(--color-muted)]">
          {tr(
            "collection.last_scan",
            { date: new Date(library.generated_at).toLocaleString() },
            "Last scanned {date}",
          )}
        </p>
      )}
    </Modal>
  );
}
