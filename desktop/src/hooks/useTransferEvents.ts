import { useEffect, useRef } from 'react';
import { listen } from '../electron-api/event';
import { useTransferStore } from '../state';
import {
  TransferProgressEvent,
  TransferScanEvent,
  TransferCompleteEvent,
  TransferErrorEvent,
} from '../types';


export function useTransferEvents() {
  const lastProgressUpdate = useRef(0);
  const pendingProgress = useRef<TransferProgressEvent | null>(null);
  const progressTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    let mounted = true;

    const unlisten = async () => {
      const unlistenFns = await Promise.all([
        listen<TransferProgressEvent>("transfer_progress", (event) => {
          if (!mounted) return;
          const { run_id, sent, total, files_sent, elapsed_secs, current_file } = event.payload;
          const transferState = useTransferStore.getState();
          if (transferState.runId && run_id !== transferState.runId) return;

          pendingProgress.current = event.payload;
          const flush = () => {
            if (!mounted) return;
            const payload = pendingProgress.current;
            if (!payload) return;
            useTransferStore.setState({
              status: "Uploading",
              sent: payload.sent,
              total: payload.total,
              files: payload.files_sent,
              elapsed: payload.elapsed_secs,
              currentFile: payload.current_file ?? "",
            });
            pendingProgress.current = null;
            progressTimer.current = null;
            lastProgressUpdate.current = Date.now();
          };
          const now = Date.now();
          const elapsed = now - lastProgressUpdate.current;
          if (elapsed >= 1000) {
            if (progressTimer.current) {
              return;
            }
            flush();
            return;
          }
          if (!progressTimer.current) {
            progressTimer.current = setTimeout(flush, 1000 - elapsed);
          }
        }),
        listen<TransferScanEvent>("transfer_scan", (event) => {
          if (!mounted) return;
          const { run_id, files_found, total_size } = event.payload;
          const transferState = useTransferStore.getState();
          if (transferState.runId && run_id !== transferState.runId) return;

          useTransferStore.setState({
            status: "Scanning",
            files: files_found,
            total: total_size,
          });
        }),
        listen<TransferCompleteEvent>("transfer_complete", (event) => {
          if (!mounted) return;
          if (progressTimer.current) {
            clearTimeout(progressTimer.current);
            progressTimer.current = null;
          }
          pendingProgress.current = null;
          const { run_id, files, bytes } = event.payload;
          const transferState = useTransferStore.getState();
          if (transferState.runId && run_id !== transferState.runId) return;

          useTransferStore.setState({
            status: "Complete",
            sent: bytes,
            total: bytes,
            files,
          });
          // Logic for history and queue should be handled separately
        }),
        listen<TransferErrorEvent>("transfer_error", (event) => {
          if (!mounted) return;
          if (progressTimer.current) {
            clearTimeout(progressTimer.current);
            progressTimer.current = null;
          }
          pendingProgress.current = null;
          const { run_id, message } = event.payload;
          const transferState = useTransferStore.getState();
          if (transferState.runId && run_id !== transferState.runId) return;
          
          useTransferStore.setState({
            status: `Error: ${message}`,
          });
        }),
      ]);
      return () => {
        if (progressTimer.current) {
          clearTimeout(progressTimer.current);
          progressTimer.current = null;
        }
        unlistenFns.forEach((fn) => fn());
      };
    };

    const cleanupPromise = unlisten();

    return () => {
      mounted = false;
      cleanupPromise.then((cleanup) => cleanup());
    };
  }, []);
}
