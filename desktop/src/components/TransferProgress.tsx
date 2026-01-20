import React from 'react';
import { useTransferStore } from '../state';

function formatBytes(bytes: number) {
  const kb = 1024;
  const mb = kb * 1024;
  const gb = mb * 1024;
  if (bytes >= gb) return `${(bytes / gb).toFixed(2)} GB`;
  if (bytes >= mb) return `${(bytes / mb).toFixed(2)} MB`;
  if (bytes >= kb) return `${(bytes / kb).toFixed(2)} KB`;
  return `${bytes} B`;
}

export const TransferProgress: React.FC = () => {
  const { status, sent, total, files, currentFile } = useTransferStore();

  const transferPercent = total > 0 ? Math.min(100, (sent / total) * 100) : 0;

  return (
    <div className="card wide">
      <header className="card-title">
        <span className="card-title-icon">▶</span>
        Transfer Progress
      </header>
      <div className="progress">
        <div
          className="progress-fill"
          style={{ width: `${transferPercent}%` }}
        />
      </div>
      <div className="progress-meta">
        <span>
          {formatBytes(sent)} / {formatBytes(total)}
        </span>
        <span>
          {files} files
        </span>
        <span>{status}</span>
      </div>
      {currentFile && (
        <div className="pill">{currentFile}</div>
      )}
    </div>
  );
};
