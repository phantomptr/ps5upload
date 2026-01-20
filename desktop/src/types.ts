// Event payload types
export type TransferProgressEvent = {
  run_id: number;
  sent: number;
  total: number;
  files_sent: number;
  elapsed_secs: number;
  current_file: string | null;
};

export type TransferScanEvent = {
  run_id: number;
  files_found: number;
  total_size: number;
};

export type TransferCompleteEvent = {
  run_id: number;
  files: number;
  bytes: number;
};

export type TransferErrorEvent = {
  run_id: number;
  message: string;
};

export type TransferLogEvent = {
  run_id: number;
  message: string;
};

// Other types
export type DirEntry = {
  name: string;
  entry_type: string;
  size: number;
  mtime?: number | null;
};
