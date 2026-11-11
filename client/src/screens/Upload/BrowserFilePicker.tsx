import { useRef, useState } from "react";
import { Button, ErrorCard } from "../../components";
import { stageBrowserFile } from "../../api/pkgUpload";
import { isStagingCancelled, noteStaged, stageWithTask } from "../../lib/browserStaging";
import { useTr } from "../../state/lang";
import { isTarArchiveName } from "../../lib/tarNames";

/** Browser files become engine paths before entering the existing review/queue. */
export default function BrowserFilePicker({ onFiles }: { onFiles: (paths: string[]) => void }) {
  const tr = useTr();
  const input = useRef<HTMLInputElement>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [percent, setPercent] = useState<number | null>(null);
  const stop = useRef<AbortController | null>(null);
  async function stage(files: File[]) {
    if (busy || !files.length) return;
    const controller = new AbortController();
    stop.current = controller;
    setBusy(true);
    setError(null);
    const paths: string[] = [];
    try {
      for (const file of files) {
        if (/\.(zip|7z|rar|[r-y]\d{2})$/i.test(file.name) || isTarArchiveName(file.name)) throw new Error(tr("browserFile.archive", "Archive uploads require the native app. Choose a plain file or disk image."));
        const staged = await stageWithTask(
          file,
          stageBrowserFile,
          (sent, total) => setPercent(total > 0 ? Math.floor((sent / total) * 100) : null),
          controller.signal,
        );
        noteStaged(staged);
        paths.push(staged.path);
      }
    } catch (e) {
      if (!isStagingCancelled(e)) setError(String(e));
    } finally {
      if (paths.length) onFiles(paths);
      setBusy(false);
      setPercent(null);
      stop.current = null;
    }
  }
  return <div className="rounded-xl border border-[var(--color-border)] p-4 space-y-2"
    onDragOver={(e) => { e.preventDefault(); e.stopPropagation(); }}
    onDrop={(e) => { e.preventDefault(); e.stopPropagation(); void stage(Array.from(e.dataTransfer.files)); }}>
    <input ref={input} type="file" multiple hidden onChange={(e) => {
      const files = Array.from(e.currentTarget.files ?? []);
      e.currentTarget.value = "";
      void stage(files);
    }} />
    <div className="flex items-center gap-2">
      <Button variant="secondary" disabled={busy} onClick={() => input.current?.click()}>
        {!busy
          ? tr("browserFile.choose", "From this device…")
          : percent === null
            ? tr("browserFile.staging", "Uploading to server…")
            : tr("browserFile.staging_percent", { percent }, `Uploading to server… ${percent}%`)}
      </Button>
      {busy && (
        <Button variant="secondary" onClick={() => stop.current?.abort()}>
          {tr("task_cancel", "Cancel")}
        </Button>
      )}
    </div>
    <p className="text-xs text-[var(--color-muted)]">{tr("browserFile.hint_copy", "Choose or drop files from this device. They upload to the server first, then appear below for review before sending to the PS5. The server deletes its copy when the upload to the PS5 is done. Keep this tab open until the first upload finishes.")}</p>
    {error && <ErrorCard title={error} />}
  </div>;
}
