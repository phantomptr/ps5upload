import { useCallback, useEffect, useRef, useState } from "react";
import { Binary, ChevronLeft, ChevronRight, FileText, PencilLine } from "lucide-react";
import { callBlob } from "../api";
import { t } from "../strings";
import { NAV_ARROW_EVENT } from "../nav";
import { Modal } from "../ui/Modal";
import { HexView } from "./viewer/HexView";
import { HEX_MAX } from "./viewer/hex";
import { gameFacts, isParamJsonPath, paramJsonText, type GameInfo } from "./viewer/gameInfo";
import { imageNeighbours } from "./viewer/images";
import { isJsonName, prettyJson, toRows, type Row } from "./viewer/text";
import { TextView } from "./viewer/TextView";
import { EDITOR_CANCEL, TextEditor } from "./viewer/TextEditor";
import { decodeEditable, stampOf, type FileStamp } from "./transfer";

/** The most the viewer reads (the helper's /api/fs/read cap). */
export const VIEW_MAX = 1024 * 1024;

/** Images are read whole, 1 MiB per request, up to this much. */
export const IMAGE_MAX = 16 * VIEW_MAX;

/**
 * Read `path` from the start, request after request, until it ends or `max`
 * bytes. `signal` (the viewer closed) stops it before the next request and
 * aborts the one in flight: each holds one of the helper's four web workers.
 */
export async function readUpTo(path: string, max: number, signal?: AbortSignal): Promise<Blob> {
  const parts: Blob[] = [];
  let at = 0;
  while (at < max) {
    if (signal?.aborted) throw new Error("aborted");
    const want = Math.min(VIEW_MAX, max - at);
    const b = await callBlob("/api/fs/read", { path, offset: at, len: want }, signal);
    parts.push(b);
    at += b.size;
    if (b.size < want) break;
  }
  return parts.length === 1 ? parts[0] : new Blob(parts);
}

/**
 * An image the viewer will not show: larger than IMAGE_MAX by its listed size,
 * or (size unknown) the read stopped at the cap — showing those bytes would be
 * a cut-off picture.
 */
export function imageTooLarge(size: number | undefined, read: number | null): boolean {
  if (size != null) return size > IMAGE_MAX;
  return read != null && read >= IMAGE_MAX;
}

/** What the viewer shows a file as: an image, text (JSON pretty printed), or hex. */
export type ViewKind = "image" | "text" | "hex";

export interface TextModel {
  rows: Row[];
  /** A .json file that did not parse (or was cut at 1 MiB): shown as written. */
  jsonRaw: boolean;
  /** sce_sys/param.json's summary. */
  game: GameInfo | null;
}

/** Decoded text as the viewer shows it (pure: tests call it). */
export function textModel(path: string, name: string, text: string): TextModel {
  let shown = text;
  let jsonRaw = false;
  if (isJsonName(name)) {
    const p = prettyJson(text);
    shown = p.text;
    jsonRaw = !p.ok;
  }
  return { rows: toRows(shown), jsonRaw, game: isParamJsonPath(path) ? paramJsonText(text) : null };
}

type Loaded =
  | { kind: "image"; url: string; size: number }
  | { kind: "text"; model: TextModel; size: number; edit: Editable | null }
  | { kind: "hex"; bytes: Uint8Array }
  | { kind: "error" }
  | { kind: "too_large" }
  | null;

/** A text file the editor can open: read whole (up to 1 MiB), valid UTF-8, no NUL; as it was then. */
interface Editable {
  text: string;
  stamp: FileStamp;
}

/** The editable form of a text file just read, or null (cut at the cap, not UTF-8, binary, or changed while read). */
export function editableOf(buf: ArrayBuffer, stamp: FileStamp | null): Editable | null {
  if (!stamp || buf.byteLength !== stamp.size || buf.byteLength > VIEW_MAX) return null;
  const text = decodeEditable(buf);
  return text == null ? null : { text, stamp };
}

/** sce_sys/param.json's summary above its text. */
export function GameSummary({ info }: { info: GameInfo }) {
  return (
    <div className="cw-gb is-summary" data-game-summary="">
      <div className="cw-gb-text">
        <strong className="cw-gb-title">{info.title ?? info.titleId ?? ""}</strong>
        <span className="cw-dim cw-gb-facts">{gameFacts(info)}</span>
        {info.contentId ? <span className="cw-dim cw-mono cw-gb-facts">{info.contentId}</span> : null}
      </div>
    </div>
  );
}

/**
 * The viewer: an image (png/jpg, up to 16 MiB; previous / next through the
 * folder's images), the first 1 MiB of a text file (line numbers, find, wrap;
 * JSON pretty printed, sce_sys/param.json summarized), or the first 64 KiB of
 * any file as hex. Bytes come from /api/fs/read with the token header: an
 * image through a blob: URL (revoked on close), text decoded as UTF-8 (bad
 * bytes become U+FFFD) and rendered as text nodes, never as HTML. ○ closes it.
 */
export function Viewer({
  path,
  name,
  kind,
  size,
  images,
  onOpenImage,
  startEditing,
  onSaved,
  onEditing,
  onClose,
}: {
  path: string;
  name: string;
  kind: ViewKind;
  /** The file's size from the folder listing, when known. */
  size?: number;
  /** The folder's images in list order (an image's previous / next). */
  images?: string[];
  onOpenImage?: (name: string) => void;
  /** Open straight in the editor (a new file). */
  startEditing?: boolean;
  /** A save went through (the folder's listing changed). */
  onSaved?: () => void;
  /** The text editor opened or closed: while it is open, Files does not change folder under it. */
  onEditing?: (editing: boolean) => void;
  onClose: () => void;
}) {
  const [loaded, setLoaded] = useState<Loaded>(null);
  const [editing, setEditing] = useState(!!startEditing);
  const [saved, setSaved] = useState(false);
  useEffect(() => {
    onEditing?.(editing);
    return () => onEditing?.(false);
  }, [editing, onEditing]);
  /** Bumped after a save: the file is read again. */
  const [reload, setReload] = useState(0);
  /** A text file shown as hex instead (the head's toggle). */
  const [asHex, setAsHex] = useState(false);
  const mode: ViewKind = kind === "text" && asHex ? "hex" : kind;
  const nb = kind === "image" && images ? imageNeighbours(images, name) : null;
  const imageRef = useRef<HTMLImageElement>(null);
  const prevImage = nb?.prev ?? null;
  const nextImage = nb?.next ?? null;

  /** The picture's blob: URL shown now: revoked when the next one replaces it, and on close. */
  const shownUrl = useRef<string | null>(null);
  const showUrl = useCallback((url: string | null) => {
    const old = shownUrl.current;
    shownUrl.current = url;
    if (old && old !== url) URL.revokeObjectURL(old);
  }, []);
  useEffect(() => () => showUrl(null), [showUrl]);

  useEffect(() => {
    let live = true;
    const stop = new AbortController();
    // The next image keeps the one shown (and the focus on it) until it arrives.
    setLoaded((l) => (mode === "image" && l?.kind === "image" ? l : null));
    if (mode === "image" && imageTooLarge(size, null)) {
      showUrl(null);
      setLoaded({ kind: "too_large" }); // nothing read: it would not be shown
      return;
    }
    const max = mode === "image" ? IMAGE_MAX : mode === "hex" ? HEX_MAX : VIEW_MAX;
    // A text file's stamp (mtime + size) is read first: a save is refused if the file changes after.
    const stamp = mode === "text" ? stampOf(path).catch(() => null) : Promise.resolve(null);
    stamp.then((st) => readUpTo(path, max, stop.signal).then((blob) => ({ blob, st }))).then(
      async ({ blob, st }) => {
        if (!live) return;
        if (mode === "image") {
          if (imageTooLarge(size, blob.size)) {
            showUrl(null);
            setLoaded({ kind: "too_large" });
            return;
          }
          const ext = name.slice(name.lastIndexOf(".") + 1).toLowerCase();
          const url = URL.createObjectURL(new Blob([blob], { type: ext === "png" ? "image/png" : "image/jpeg" }));
          setLoaded({ kind: "image", url, size: blob.size });
          showUrl(url);
          return;
        }
        const buf = await new Response(blob).arrayBuffer();
        if (!live) return;
        if (mode === "hex") {
          setLoaded({ kind: "hex", bytes: new Uint8Array(buf, 0, Math.min(buf.byteLength, HEX_MAX)) });
          return;
        }
        const text = new TextDecoder("utf-8", { fatal: false }).decode(buf);
        setLoaded({ kind: "text", model: textModel(path, name, text), size: blob.size, edit: editableOf(buf, st) });
      },
      () => {
        if (!live) return;
        showUrl(null);
        setLoaded({ kind: "error" });
      },
    );
    return () => {
      live = false;
      stop.abort();
    };
  }, [path, name, mode, size, showUrl, reload]);

  // ← / → (D-pad or keys) on the picture itself: previous / next image. The nav layer hands
  // left/right on a [data-nav-arrows] element to it as a NAV_ARROW_EVENT instead of moving focus.
  const loadedImage = loaded?.kind === "image";
  useEffect(() => {
    const img = imageRef.current;
    if (!img || !onOpenImage) return;
    const onArrow = (e: Event) => {
      const dir = (e as CustomEvent<"left" | "right">).detail;
      const to = dir === "left" ? prevImage : nextImage;
      if (to) onOpenImage(to);
    };
    img.addEventListener(NAV_ARROW_EVENT, onArrow);
    return () => img.removeEventListener(NAV_ARROW_EVENT, onArrow);
  }, [prevImage, nextImage, onOpenImage, loadedImage]);

  const edit = loaded?.kind === "text" ? loaded.edit : null;
  const inEditor = editing && mode === "text" && edit != null;
  // ○ in the editor is its Cancel (unsaved changes are asked about); else it closes the viewer.
  const onBack = useCallback(() => {
    const cancel = inEditor ? document.querySelector<HTMLButtonElement>(EDITOR_CANCEL) : null;
    if (cancel) cancel.click();
    else onClose();
  }, [inEditor, onClose]);

  return (
    <Modal label={name} className={`cw-viewer${inEditor ? " is-editing" : ""}`} onBack={onBack}>
      <div className="cw-viewer-head">
        <h2 className="cw-file-name">{name}</h2>
        {nb && nb.total > 1 ? (
          <span className="cw-dim cw-viewer-count" data-image-count="">
            {t("cw_files_vw_image_at", { at: nb.at + 1, n: nb.total })}
          </span>
        ) : null}
        {saved && !inEditor ? (
          <span className="cw-dim" role="status" data-saved="">
            {t("cw_files_edit_saved")}
          </span>
        ) : null}
        {mode === "text" && edit && !inEditor ? (
          <button
            type="button"
            className="cw-focusable cw-button"
            data-action="edit"
            onClick={() => {
              setSaved(false);
              setEditing(true);
            }}
          >
            <PencilLine aria-hidden />
            <span>{t("cw_files_edit")}</span>
          </button>
        ) : null}
        {kind === "text" && !inEditor ? (
          <button
            type="button"
            className="cw-focusable cw-button"
            data-action="as-hex"
            aria-pressed={asHex}
            onClick={() => setAsHex((h) => !h)}
          >
            {asHex ? <FileText aria-hidden /> : <Binary aria-hidden />}
            <span>{t(asHex ? "cw_files_vw_as_text" : "cw_files_vw_as_hex")}</span>
          </button>
        ) : null}
        <button
          type="button"
          className="cw-focusable cw-button"
          data-nav-default={inEditor ? undefined : ""}
          data-action="viewer-close"
          onClick={onClose}
        >
          <span>{t("cw_files_close")}</span>
        </button>
      </div>
      <div className={`cw-viewer-body is-${mode}`}>
        {loaded == null ? <p className="cw-dim">{t("cw_home_loading")}</p> : null}
        {loaded?.kind === "error" ? <p className="cw-field-error">{t("cw_files_err_read")}</p> : null}
        {loaded?.kind === "too_large" ? <p className="cw-dim">{t("cw_files_too_large")}</p> : null}
        {mode === "image" && nb && onOpenImage ? (
          <button
            type="button"
            className="cw-focusable cw-button cw-image-nav"
            data-action="image-prev"
            aria-label={t("cw_files_vw_image_prev")}
            data-tip={t("cw_files_vw_image_prev")}
            aria-disabled={!nb.prev || undefined}
            onClick={() => nb.prev && onOpenImage(nb.prev)}
          >
            <ChevronLeft aria-hidden />
          </button>
        ) : null}
        {loaded?.kind === "image" ? (
          <img
            ref={imageRef}
            className="cw-focusable"
            tabIndex={0}
            src={loaded.url}
            alt={name}
            data-image-view=""
            data-nav-arrows={onOpenImage ? "" : undefined}
          />
        ) : null}
        {mode === "image" && nb && onOpenImage ? (
          <button
            type="button"
            className="cw-focusable cw-button cw-image-nav"
            data-action="image-next"
            aria-label={t("cw_files_vw_image_next")}
            data-tip={t("cw_files_vw_image_next")}
            aria-disabled={!nb.next || undefined}
            onClick={() => nb.next && onOpenImage(nb.next)}
          >
            <ChevronRight aria-hidden />
          </button>
        ) : null}
        {loaded?.kind === "text" && inEditor && edit ? (
          <TextEditor
            key={`${path}:${reload}`}
            path={path}
            text={edit.text}
            stamp={edit.stamp}
            onClose={() => setEditing(false)}
            onSaved={() => {
              setEditing(false);
              setSaved(true);
              setReload((n) => n + 1);
              onSaved?.();
            }}
          />
        ) : null}
        {loaded?.kind === "text" && editing && !edit ? (
          <p className="cw-msg is-info" data-edit-refused="">
            {t(loaded.size >= VIEW_MAX ? "cw_files_edit_too_large" : "cw_files_edit_not_text")}
          </p>
        ) : null}
        {loaded?.kind === "text" && !inEditor ? (
          <>
            {loaded.model.game ? <GameSummary info={loaded.model.game} /> : null}
            {loaded.size >= VIEW_MAX ? <p className="cw-dim">{t("cw_files_truncated")}</p> : null}
            {loaded.model.jsonRaw ? <p className="cw-dim">{t("cw_files_vw_json_raw")}</p> : null}
            <TextView key={path} rows={loaded.model.rows} />
          </>
        ) : null}
        {loaded?.kind === "hex" ? <HexView key={path} bytes={loaded.bytes} size={size} /> : null}
      </div>
    </Modal>
  );
}
