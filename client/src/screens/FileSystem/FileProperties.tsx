import { useEffect, useState } from "react";
import { Button } from "../../components/Button";
import { Input } from "../../components/Input";
import { Modal } from "../../components/Modal";
import { fetchConsoleStat, type ConsolePathStat } from "../../api/consoleFiles";
import { fsChmod } from "../../api/ps5";
import { consoleAddr } from "../../lib/addr";
import {
  keepsPermissions,
  modeLetters,
  modeText,
  parseMode,
} from "../../lib/filePermissions";
import { useTr } from "../../state/lang";
import { formatNumber, formatDate } from "../../lib/formatDate";

interface Props {
  path: string;
  stat: ConsolePathStat | null;
  mode: string;
  recursive: boolean;
  busy: boolean;
  error: string | null;
  fsType?: string;
  onMode: (value: string) => void;
  onRecursive: (value: boolean) => void;
  onApply: () => void;
  onClose: () => void;
}

export function FilePropertiesView(p: Props) {
  const tr = useTr();
  return (
    <Modal
      open
      title={tr("fs_properties", undefined, "Properties & permissions")}
      onClose={() => {
        if (!p.busy) p.onClose();
      }}
    >
      <div className="space-y-3 text-sm">
        <p className="break-all font-mono">{p.path}</p>
        {p.stat && (
          <dl className="grid grid-cols-2 gap-2">
            <dt>{tr("cw_files_info_type", undefined, "Type")}</dt>
            <dd>
              {tr(`cw_files_kind_${p.stat.kind}`, undefined, p.stat.kind)}
            </dd>
            <dt>{tr("cw_files_info_size", undefined, "Size (bytes)")}</dt>
            <dd>{formatNumber(p.stat.size)} B</dd>
            <dt>{tr("cw_files_info_modified", undefined, "Modified")}</dt>
            <dd>
              {p.stat.mtime
                ? formatDate(p.stat.mtime * 1000)
                : "—"}
            </dd>
            <dt>
              {tr("cw_files_info_mode", undefined, "Current permissions")}
            </dt>
            <dd className="font-mono">
              {modeText(p.stat.mode)} · {modeLetters(p.stat.mode)}
            </dd>
          </dl>
        )}
        {!keepsPermissions(p.fsType) && (
          <p role="note">
            {tr(
              "cw_files_perm_not_kept",
              { fs: p.fsType ?? "" },
              "This file system does not keep Unix permissions; changes may not persist.",
            )}
          </p>
        )}
        <Input
          label={tr(
            "fs_properties_mode",
            undefined,
            "Permissions (octal, e.g. 0444 or 0755)",
          )}
          className="font-mono"
          value={p.mode}
          onChange={(e) => p.onMode(e.target.value)}
          disabled={p.busy || !p.stat}
          maxLength={4}
          inputMode="numeric"
        />
        {p.stat?.kind === "dir" && (
          <label className="flex gap-2">
            <input
              type="checkbox"
              checked={p.recursive}
              onChange={(e) => p.onRecursive(e.target.checked)}
              disabled={p.busy}
            />
            {tr(
              "cw_files_perm_recursive",
              undefined,
              "Apply to everything inside",
            )}
          </label>
        )}
        {p.error && (
          <p role="alert" className="text-[var(--color-bad)]">
            {p.error}
          </p>
        )}
        <div className="flex justify-end gap-3">
          <Button variant="secondary" onClick={p.onClose} disabled={p.busy}>
            {tr("cancel", undefined, "Cancel")}
          </Button>
          <Button
            variant="primary"
            onClick={p.onApply}
            disabled={p.busy || !p.stat || parseMode(p.mode) === null}
          >
            {tr("cw_files_perm_apply", undefined, "Apply")}
          </Button>
        </div>
      </div>
    </Modal>
  );
}

export function FileProperties({
  host,
  path,
  fsType,
  onClose,
  onChanged,
}: {
  host: string;
  path: string;
  fsType?: string;
  onClose: () => void;
  onChanged: () => void;
}) {
  const tr = useTr();
  const [stat, setStat] = useState<ConsolePathStat | null>(null);
  const [mode, setMode] = useState("");
  const [recursive, setRecursive] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let gone = false;
    void fetchConsoleStat(host, path).then(
      (s) => {
        if (!gone) {
          setStat(s);
          setMode(modeText(s.mode));
        }
      },
      (e: unknown) => {
        if (!gone) setError(String(e));
      },
    );
    return () => {
      gone = true;
    };
  }, [host, path]);
  const apply = async () => {
    const value = parseMode(mode);
    if (value === null || !stat || busy) return;
    setBusy(true);
    setError(null);
    try {
      await fsChmod(
        consoleAddr(host),
        path,
        modeText(value),
        stat.kind === "dir" && recursive,
      );
      const fresh = await fetchConsoleStat(host, path);
      setStat(fresh);
      setMode(modeText(fresh.mode));
      if (keepsPermissions(fsType) && (fresh.mode & 0o777) !== value)
        setError(
          tr(
            "fs_chmod_failed",
            undefined,
            "The helper returned a different permission mode. Check whether this path is read-only.",
          ),
        );
      onChanged();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <FilePropertiesView
      path={path}
      fsType={fsType}
      stat={stat}
      mode={mode}
      recursive={recursive}
      busy={busy}
      error={error}
      onMode={setMode}
      onRecursive={setRecursive}
      onApply={() => void apply()}
      onClose={onClose}
    />
  );
}
