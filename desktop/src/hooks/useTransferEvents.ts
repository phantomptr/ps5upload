import { useEffect, useRef } from 'react';
import { listen } from '@tauri-apps/api/event';
import { useTransferStore } from '../state';
import {
  TransferProgressEvent,
  TransferScanEvent,
  TransferCompleteEvent,
  TransferErrorEvent,
} from '../types';


export function useTransferEvents() {
  const lastProgressUpdate = useRef(0);

  useEffect(() => {
    let mounted = true;

    const unlisten = async () => {
      const unlistenFns = await Promise.all([
        listen<TransferProgressEvent>("transfer_progress", (event) => {
          if (!mounted) return;
          const now = Date.now();
          if (now - lastProgressUpdate.current < 100) {
            return;
          }
          lastProgressUpdate.current = now;
          const { run_id, sent, total, files_sent, elapsed_secs, current_file } = event.payload;
          const transferState = useTransferStore.getState();
          if (transferState.runId && run_id !== transferState.runId) return;

          useTransferStore.setState({
            status: "Uploading",
            sent,
            total,
            files: files_sent,
            elapsed: elapsed_secs,
            currentFile: current_file ?? "",
          });
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
          const { run_id, message } = event.payload;
          const transferState = useTransferStore.getState();
          if (transferState.runId && run_id !== transferState.runId) return;
          
          useTransferStore.setState({
            status: `Error: ${message}`,
          });
        }),
      ]);
      return () => {
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
