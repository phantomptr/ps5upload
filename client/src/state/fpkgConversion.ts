import { create } from "zustand";

import { fpkg, type FpkgBuildRequest } from "../api/fpkg";
import { jobCancel, jobStatus, type JobSnapshot } from "../api/ps5";
import { pushNotification } from "./notifications";

interface ConversionState {
  jobId: string | null;
  job: JobSnapshot | null;
  error: string | null;
  starting: boolean;
  start: (request: FpkgBuildRequest) => Promise<void>;
  cancel: () => Promise<void>;
}

function schedulePoll(jobId: string, failures = 0) {
  setTimeout(async () => {
    try {
      const snapshot = await jobStatus(jobId);
      if (useFpkgConversion.getState().jobId !== jobId) return;
      useFpkgConversion.setState({ job: snapshot });
      if (snapshot.status === "running") {
        schedulePoll(jobId);
      } else {
        useFpkgConversion.setState({ jobId: null });
        if (snapshot.status === "done") {
          pushNotification("success", "FPKG conversion complete", {
            body: snapshot.dest,
            link: "/convert",
          });
        } else if (snapshot.status === "failed") {
          pushNotification("error", "FPKG conversion failed", {
            body: snapshot.error,
            link: "/convert",
          });
        }
      }
    } catch (error) {
      if (useFpkgConversion.getState().jobId !== jobId) return;
      if (failures < 5) {
        schedulePoll(jobId, failures + 1);
        return;
      }
      const message = error instanceof Error ? error.message : String(error);
      useFpkgConversion.setState({ error: message, jobId: null });
      pushNotification("error", "FPKG conversion status unavailable", {
        body: message,
        link: "/convert",
      });
    }
  }, 500);
}

export const useFpkgConversion = create<ConversionState>((set, get) => ({
  jobId: null,
  job: null,
  error: null,
  starting: false,
  start: async (request) => {
    if (get().starting || get().jobId) return;
    set({ starting: true, error: null, job: null });
    try {
      const { job_id } = await fpkg.build(request);
      set({ jobId: job_id });
      schedulePoll(job_id);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      set({ error: message });
      pushNotification("error", "FPKG conversion failed", { body: message, link: "/convert" });
    } finally {
      set({ starting: false });
    }
  },
  cancel: async () => {
    const jobId = get().jobId;
    if (jobId) await jobCancel(jobId);
  },
}));
