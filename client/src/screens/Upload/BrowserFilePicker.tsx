import { useRef, useState } from "react";
import { Button, ErrorCard } from "../../components";
import { stageBrowserFile } from "../../api/pkgUpload";
import { useTr } from "../../state/lang";

/** Browser files become engine paths before entering the existing review/queue. */
export default function BrowserFilePicker({ onFiles }: { onFiles: (paths: string[]) => void }) {
  const tr = useTr();
  const input = useRef<HTMLInputElement>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  async function stage(files: File[]) {
    if (busy || !files.length) return;
    setBusy(true);
    setError(null);
    const paths: string[] = [];
    try {
      for (const file of files) {
        if (/\.(zip|7z|rar|[r-y]\d{2})$/i.test(file.name)) throw new Error(tr("browserFile.archive", "Archive uploads require the native app. Choose a plain file or disk image."));
        paths.push((await stageBrowserFile(file)).path);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      if (paths.length) onFiles(paths);
      setBusy(false);
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
    <Button variant="secondary" disabled={busy} onClick={() => input.current?.click()}>
      {busy ? tr("browserFile.staging", "Uploading to server…") : tr("browserFile.choose", "From this device…")}
    </Button>
    <p className="text-xs text-[var(--color-muted)]">{tr("browserFile.hint", "Choose or drop files from this device. They upload to the server first, then appear below for review before sending to the PS5. Keep this tab open until that upload finishes.")}</p>
    {error && <ErrorCard title={error} />}
  </div>;
}
