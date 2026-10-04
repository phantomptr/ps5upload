/**
 * "Resuming: K of T files already present (X); U to send" — the figures for the line the
 * Upload screen shows when the console already has part of the job (design 015/02, #365).
 * `skipped*` are what the console reported as already in place when the job opened; the
 * totals are the whole job. Null when nothing is present (a fresh upload shows no such line).
 */
export interface ResumeSummary {
  done: number;
  total: number;
  haveBytes: number;
  sendBytes: number;
}

export function resumeSummary(
  skippedFiles: number,
  totalFiles: number,
  skippedBytes: number,
  totalBytes: number,
): ResumeSummary | null {
  if (skippedFiles <= 0 && skippedBytes <= 0) return null;
  const total = totalFiles > 0 ? totalFiles : skippedFiles;
  const haveBytes = totalBytes > 0 ? Math.min(skippedBytes, totalBytes) : skippedBytes;
  return {
    done: Math.min(skippedFiles, total),
    total,
    haveBytes,
    sendBytes: Math.max(0, totalBytes - haveBytes),
  };
}
