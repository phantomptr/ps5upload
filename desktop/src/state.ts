import { create } from 'zustand';

export type TransferState = {
  status: string;
  sent: number;
  total: number;
  files: number;
  elapsed: number;
  currentFile: string;
  runId: number | null;
  source: string;
  dest: string;
  viaQueue: boolean;
  startedAt: number | null;
};

export const useTransferStore = create<TransferState>()((set) => ({
  status: "Idle",
  sent: 0,
  total: 0,
  files: 0,
  elapsed: 0,
  currentFile: "",
  runId: null,
  source: "",
  dest: "",
  viaQueue: false,
  startedAt: null,
}));
