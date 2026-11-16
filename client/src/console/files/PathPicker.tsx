import { useCallback, useEffect, useRef, useState } from "react";
import { ArrowUp, Check, File as FileIcon, Folder, HardDrive } from "lucide-react";
import { isDrive, volumeLabel, type Volume } from "../home/Home";
import { focusElement } from "../nav";
import { t } from "../strings";
import { Modal } from "../ui/Modal";
import { joinPath, upFrom } from "./move";
import { useDir } from "./useDir";

/**
 * Copy to… / Move to…: drives, then folders; "Here" picks the folder shown.
 * ○ goes up one level, and from the drive list closes the picker. With
 * `pickFile`, the files it accepts are listed under the folders and pressing
 * one picks it (Convert's source: a game folder, or an image file).
 */
export function PathPicker({
  title,
  subtitle,
  volumes,
  start,
  onPick,
  onCancel,
  pickFile,
}: {
  title: string;
  /** What is being copied or moved (a name, or "3 items"). */
  subtitle: string;
  volumes: Volume[];
  /** The folder to open at (the current one). */
  start: string | null;
  onPick: (dir: string) => void;
  onCancel: () => void;
  /** Files with a name it accepts are listed too, and picked (their path) when pressed. */
  pickFile?: (name: string) => boolean;
}) {
  const [path, setPath] = useState<string | null>(start);
  const dir = useDir(path);
  const list = useRef<HTMLDivElement>(null);
  const drives = volumes.filter((v) => isDrive(v) && v.writable);
  const folders = dir.entries.filter((e) => e.kind === "dir");
  const files = pickFile ? dir.entries.filter((e) => e.kind === "file" && pickFile(e.name)) : [];

  // On open and after a level change, focus the first row (else "Here").
  const moved = useRef(true);
  useEffect(() => {
    if (!moved.current || (path != null && !dir.loaded)) return;
    moved.current = false;
    const el = list.current?.querySelector<HTMLElement>("[data-pick]") ?? list.current?.querySelector<HTMLElement>("[data-pick-here]");
    if (el) focusElement(el);
  }, [path, dir.loaded]);

  const go = (p: string | null) => {
    moved.current = true;
    setPath(p);
  };
  const back = useCallback(() => {
    if (path == null) onCancel();
    else {
      moved.current = true;
      setPath(upFrom(path, volumes));
    }
  }, [path, volumes, onCancel]);

  return (
    <Modal label={title} className="cw-picker" onBack={back}>
      <h2>{title}</h2>
      <p className="cw-dim">{subtitle}</p>
      <div ref={list}>
        <div className="cw-picker-bar">
          <span className="cw-path cw-mono">{path ?? t("cw_files_pick_drive")}</span>
          {path != null ? (
            <button type="button" className="cw-focusable cw-button" onClick={back}>
              <ArrowUp aria-hidden />
              <span>{t("cw_files_up")}</span>
            </button>
          ) : null}
        </div>
        <ul className="cw-picker-list">
          {path == null
            ? drives.map((v) => (
                <li key={v.path}>
                  <button
                    type="button"
                    className="cw-focusable cw-sheet-item"
                    data-pick={v.path}
                    data-nav-default=""
                    onClick={() => go(v.path)}
                  >
                    <HardDrive aria-hidden />
                    <span>{volumeLabel(v)}</span>
                    <span className="cw-dim cw-mono">{v.path}</span>
                  </button>
                </li>
              ))
            : folders.map((e) => (
                <li key={e.name}>
                  <button
                    type="button"
                    className="cw-focusable cw-sheet-item"
                    data-pick={e.name}
                    onClick={() => go(joinPath(path, e.name))}
                  >
                    <Folder aria-hidden />
                    <span>{e.name}</span>
                  </button>
                </li>
              ))}
          {path != null
            ? files.map((e) => (
                <li key={`f:${e.name}`}>
                  <button
                    type="button"
                    className="cw-focusable cw-sheet-item"
                    data-pick-file={e.name}
                    onClick={() => onPick(joinPath(path, e.name))}
                  >
                    <FileIcon aria-hidden />
                    <span>{e.name}</span>
                  </button>
                </li>
              ))
            : null}
        </ul>
        {path != null && !dir.loaded ? <p className="cw-dim">{t("cw_home_loading")}</p> : null}
        {path != null && dir.loaded && !folders.length && !files.length && !dir.more ? (
          <p className="cw-dim">
            {t("cw_files_no_folders")} {t("cw_files_no_folders_next")}
          </p>
        ) : null}
        {path != null && dir.more ? (
          <button type="button" className="cw-focusable cw-button" onFocus={dir.loadMore} onClick={dir.loadMore}>
            <span>{t("cw_files_more")}</span>
          </button>
        ) : null}
        <div className="cw-dialog-buttons">
          <button type="button" className="cw-focusable cw-button" onClick={onCancel}>
            <span>{t("cw_dialog_cancel")}</span>
          </button>
          <button
            type="button"
            className="cw-focusable cw-button is-primary"
            data-pick-here=""
            disabled={path == null}
            onClick={() => {
              if (path != null) onPick(path);
            }}
          >
            <Check aria-hidden />
            <span>{t("cw_files_here")}</span>
          </button>
        </div>
      </div>
    </Modal>
  );
}
