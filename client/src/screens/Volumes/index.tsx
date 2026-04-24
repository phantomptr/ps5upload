import { useCallback, useEffect, useMemo, useState } from "react";
import { HardDrive, FileArchive, Unplug, RefreshCw } from "lucide-react";

import { useConnectionStore, PS5_PAYLOAD_PORT } from "../../state/connection";
import { fetchVolumes, fsUnmount, type Volume } from "../../api/ps5";
import { PageHeader, EmptyState, ErrorCard, Button } from "../../components";
import { humanizePs5Error } from "../../lib/humanizeError";
import { useTr } from "../../state/lang";

/** Path prefix for volumes our FS_MOUNT creates. Showing an Unmount
 *  button only for these keeps us from accidentally offering to
 *  unmount /data or /mnt/ext0 — the payload rejects that anyway,
 *  but hiding the button avoids a confusing error. Must match
 *  FS_MOUNT_BASE in payload/src/runtime.c. */
const PS5UPLOAD_MOUNT_PREFIX = "/mnt/ps5upload/";

function formatBytes(n: number): string {
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v.toFixed(v >= 100 ? 0 : v >= 10 ? 1 : 2)} ${units[i]}`;
}

export default function VolumesScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const [volumes, setVolumes] = useState<Volume[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [unmountingPath, setUnmountingPath] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    if (!host?.trim()) return;
    setLoading(true);
    setError(null);
    try {
      const list = await fetchVolumes(`${host}:${PS5_PAYLOAD_PORT}`);
      setVolumes(list);
    } catch (e) {
      const raw = e instanceof Error ? e.message : String(e);
      // Run through the shared humanizer so transient
      // `fs_list_volumes_getmntinfo_failed` payload errors surface
      // as a retryable hint instead of a raw internal string.
      setError(humanizePs5Error(raw));
      setVolumes(null);
    } finally {
      setLoading(false);
    }
  }, [host]);

  const handleUnmount = async (mountPoint: string) => {
    if (!host?.trim()) return;
    if (
      !confirm(
        `Unmount ${mountPoint}? Anything referencing files inside will break until you re-mount.`,
      )
    ) {
      return;
    }
    setUnmountingPath(mountPoint);
    setError(null);
    try {
      await fsUnmount(`${host}:${PS5_PAYLOAD_PORT}`, mountPoint);
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setUnmountingPath(null);
    }
  };

  useEffect(() => {
    if (payloadStatus === "up") refresh();
  }, [payloadStatus, refresh]);

  const { storageDrives, mountedImages } = useMemo(() => {
    const storage: Volume[] = [];
    const mounted: Volume[] = [];
    for (const v of volumes ?? []) {
      if (v.path.startsWith(PS5UPLOAD_MOUNT_PREFIX)) {
        mounted.push(v);
      } else {
        storage.push(v);
      }
    }
    return { storageDrives: storage, mountedImages: mounted };
  }, [volumes]);

  return (
    <div className="p-6">
      <PageHeader
        icon={HardDrive}
        title={tr("volumes", undefined, "Volumes")}
        loading={loading}
        description={tr(
          "volumes_description",
          undefined,
          "Storage drives and any disk images currently mounted on your PS5.",
        )}
        right={
          <Button
            variant="secondary"
            size="sm"
            leftIcon={<RefreshCw size={12} />}
            onClick={refresh}
            disabled={loading || !host?.trim()}
            loading={loading}
          >
            {tr("refresh", undefined, "Refresh")}
          </Button>
        }
      />

      {error && (
        <div className="mb-4">
          <ErrorCard
            title={tr("volumes_read_error", undefined, "Couldn't read volumes")}
            detail={error}
          />
        </div>
      )}

      {volumes === null && !loading && !error && (
        <EmptyState
          message={tr(
            "library_waiting",
            undefined,
            "Waiting for the PS5 payload to become reachable…",
          )}
        />
      )}

      {volumes && volumes.length === 0 && (
        <EmptyState
          icon={HardDrive}
          size="hero"
          title={tr("volumes_empty_title", undefined, "No volumes visible")}
          message={tr(
            "volumes_empty_message",
            undefined,
            "The payload didn't return any writable drives. Make sure it's loaded and your PS5 has storage attached.",
          )}
        />
      )}

      {/* Mounted images section — deliberately at the top since this
          is the more actionable / transient set of volumes. Users
          typically open Volumes to unmount something or confirm a
          mount succeeded, not to inspect permanent drives. */}
      {mountedImages.length > 0 && (
        <section className="mb-6">
          <header className="mb-3 flex items-center gap-2 text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
            <FileArchive size={13} />
            Mounted disk images
            <span className="text-[10px] text-[var(--color-muted)]">
              · {mountedImages.length}
            </span>
          </header>
          <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
            {mountedImages.map((v) => (
              <MountedImageCard
                key={v.path}
                volume={v}
                onUnmount={() => handleUnmount(v.path)}
                unmounting={unmountingPath === v.path}
                anyUnmountInFlight={unmountingPath !== null}
              />
            ))}
          </div>
        </section>
      )}

      {/* Storage drives — the persistent set: internal SSD, M.2,
          USB. Unmountable only if it's one of ours, which shouldn't
          happen in this section — kept read-only. */}
      {storageDrives.length > 0 && (
        <section>
          <header className="mb-3 flex items-center gap-2 text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
            <HardDrive size={13} />
            Storage drives
            <span className="text-[10px] text-[var(--color-muted)]">
              · {storageDrives.length}
            </span>
          </header>
          <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
            {storageDrives.map((v) => (
              <StorageCard key={v.path} volume={v} />
            ))}
          </div>
        </section>
      )}
    </div>
  );
}

/** Mounted disk image — accent border, prominent Unmount button,
 *  surfaces the backing .exfat/.ffpkg path so the user can always
 *  tell which file is feeding this mount. */
function MountedImageCard({
  volume: v,
  onUnmount,
  unmounting,
  anyUnmountInFlight,
}: {
  volume: Volume;
  onUnmount: () => void;
  unmounting: boolean;
  anyUnmountInFlight: boolean;
}) {
  const pct =
    v.total_bytes > 0
      ? Math.max(0, Math.min(100, 100 - (v.free_bytes / v.total_bytes) * 100))
      : 0;
  const name = v.path.slice(PS5UPLOAD_MOUNT_PREFIX.length);
  return (
    <article className="flex flex-col gap-3 rounded-lg border border-[var(--color-accent)] bg-[var(--color-surface-2)] p-4">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0 flex-1">
          <div className="truncate text-sm font-semibold">{name}</div>
          <div className="mt-0.5 truncate font-mono text-[11px] text-[var(--color-muted)]">
            {v.path}
          </div>
          {v.source_image && (
            <div
              className="mt-2 truncate font-mono text-[11px] text-[var(--color-muted)]"
              title={v.source_image}
            >
              ← {v.source_image}
            </div>
          )}
        </div>
        <span className="shrink-0 rounded-full border border-[var(--color-accent)] bg-[var(--color-accent-soft)] px-2 py-0.5 text-[10px] font-medium uppercase tracking-wide text-[var(--color-accent)]">
          mounted
        </span>
      </div>

      {v.total_bytes > 0 && (
        <div>
          <div className="mb-1 flex items-baseline justify-between text-xs text-[var(--color-muted)]">
            <span>
              {formatBytes(v.free_bytes)} free of {formatBytes(v.total_bytes)}
            </span>
            <span className="tabular-nums">{pct.toFixed(0)}% used</span>
          </div>
          <div className="h-1.5 w-full overflow-hidden rounded-full bg-[var(--color-surface-3)]">
            <div
              className="h-full bg-[var(--color-accent)] transition-[width] duration-300"
              style={{ width: `${pct}%` }}
            />
          </div>
        </div>
      )}

      <div className="flex items-center justify-between gap-2 text-[11px] text-[var(--color-muted)]">
        <span>
          {v.fs_type} · {v.writable ? "rw" : "ro"}
        </span>
        <Button
          variant="secondary"
          size="sm"
          leftIcon={<Unplug size={12} />}
          onClick={onUnmount}
          disabled={anyUnmountInFlight}
          loading={unmounting}
        >
          Unmount
        </Button>
      </div>
    </article>
  );
}

/** Permanent storage drive. No unmount action — these are
 *  internal/USB drives the user shouldn't be unmounting from here. */
function StorageCard({ volume: v }: { volume: Volume }) {
  const pct =
    v.total_bytes > 0
      ? Math.max(0, Math.min(100, 100 - (v.free_bytes / v.total_bytes) * 100))
      : 0;
  return (
    <article className="flex flex-col gap-3 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="truncate font-mono text-sm">{v.path}</div>
          <div className="mt-0.5 text-[11px] text-[var(--color-muted)]">
            {v.mount_from ? `${v.mount_from} · ` : ""}
            {v.fs_type}
            {" · "}
            {v.writable ? "rw" : "ro"}
          </div>
        </div>
        {!v.writable && (
          <span className="shrink-0 rounded-full border border-[var(--color-border)] bg-[var(--color-surface-3)] px-2 py-0.5 text-[10px] uppercase tracking-wide text-[var(--color-muted)]">
            read-only
          </span>
        )}
      </div>

      {v.total_bytes > 0 && (
        <div>
          <div className="mb-1 flex items-baseline justify-between text-xs text-[var(--color-muted)]">
            <span>
              {formatBytes(v.free_bytes)} free of {formatBytes(v.total_bytes)}
            </span>
            <span className="tabular-nums">{pct.toFixed(0)}% used</span>
          </div>
          <div className="h-1.5 w-full overflow-hidden rounded-full bg-[var(--color-surface-3)]">
            <div
              className={`h-full transition-[width] duration-300 ${
                pct >= 95
                  ? "bg-[var(--color-bad)]"
                  : pct >= 80
                    ? "bg-[var(--color-warn)]"
                    : "bg-[var(--color-accent)]"
              }`}
              style={{ width: `${pct}%` }}
            />
          </div>
        </div>
      )}
    </article>
  );
}
