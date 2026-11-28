// Mirrors into the task store the long operations that kept their state only in their own
// store, so the activity bar shows them too: sending the helper, a payload playlist run, and
// this app downloading its update. Subscriptions only; nothing here polls.
//
// Installed once from taskWiring (process lifetime), like the other bridges.

import { trStatic } from "../lib/trStatic";
import { useHelperSendStore, type HelperSendState } from "./helperSend";
import { usePayloadPlaylistsStore } from "./payloadPlaylists";
import { useTaskStore } from "./tasks";
import { useUpdateStore, type UpdatePhase } from "./update";
import type { PlaylistRunStatus } from "../lib/playlistOps";

const fill = (text: string, vars: Record<string, string | number>) =>
  text.replace(/\{(\w+)\}/g, (m, k: string) => (k in vars ? String(vars[k]) : m));

/** Per-host transitions of a `byHost` record into tasks: `begin` when a host appears busy,
 *  `progress` while it stays busy, `end` when it leaves. Exported for tests. */
export function helperSendTransitions(
  prev: Record<string, HelperSendState>,
  next: Record<string, HelperSendState>,
): { host: string; event: "begin" | "progress" | "done" | "failed"; state?: HelperSendState }[] {
  const out: { host: string; event: "begin" | "progress" | "done" | "failed"; state?: HelperSendState }[] = [];
  const hosts = new Set([...Object.keys(prev), ...Object.keys(next)]);
  for (const host of hosts) {
    const a = prev[host];
    const b = next[host];
    if (a === b) continue;
    const wasBusy = a?.state === "busy";
    if (b?.state === "busy") {
      out.push({ host, event: wasBusy && a.startedAtMs === b.startedAtMs ? "progress" : "begin", state: b });
    } else if (wasBusy) {
      out.push({ host, event: b?.state === "fail" ? "failed" : "done", state: b });
    }
  }
  return out;
}

function installHelperSendBridge() {
  const ids = new Map<string, string>();
  useHelperSendStore.subscribe((s, p) => {
    if (s.byHost === p.byHost) return;
    const tasks = useTaskStore.getState();
    for (const t of helperSendTransitions(p.byHost, s.byHost)) {
      let id = ids.get(t.host);
      if (t.event === "begin") {
        // A new send replaces one we still hold (the user pressed Send again).
        if (id) tasks.finishTask(id, "cancelled");
        id = tasks.registerTask({
          kind: "helper-send",
          origin: "helper.send",
          label: trStatic("activity_helper_send_label", "Send the helper"),
          consoleId: t.host,
        });
        ids.set(t.host, id);
      }
      if (!id) continue;
      if (t.event === "begin" || t.event === "progress") {
        if (t.state?.msg) tasks.updateTask(id, { stage: t.state.msg });
        continue;
      }
      if (t.event === "done") tasks.finishTask(id, "done");
      else
        tasks.finishTask(id, "failed", {
          lastError: { code: "HELPER_SEND", message: t.state?.msg || "failed", recoverable: true },
        });
      ids.delete(t.host);
    }
  });
}

/** The task a playlist run status asks for: running with step progress, or an ending. */
export function playlistTaskPatch(
  status: PlaylistRunStatus,
  steps: number,
): { kind: "live"; step: number } | { kind: "done" } | { kind: "failed"; error: string } | { kind: "stopped" } {
  switch (status.kind) {
    case "running":
    case "sleeping":
      return { kind: "live", step: status.stepIndex };
    case "done":
      return status.failureCount > 0 && status.failureCount >= steps
        ? { kind: "failed", error: status.failures[0]?.error ?? "failed" }
        : { kind: "done" };
    case "failed":
      return { kind: "failed", error: status.error };
    default:
      return { kind: "stopped" };
  }
}

function installPlaylistBridge() {
  const ids = new Map<string, string>();
  usePayloadPlaylistsStore.subscribe((s, p) => {
    if (s.runStatusByHost === p.runStatusByHost) return;
    const tasks = useTaskStore.getState();
    for (const [host, status] of Object.entries(s.runStatusByHost)) {
      if (status === p.runStatusByHost[host]) continue;
      const playlistId = status.kind === "idle" ? null : status.playlistId;
      const playlist = playlistId ? s.playlists.find((pl) => pl.id === playlistId) : undefined;
      const steps = playlist?.steps.length ?? 0;
      const patch = playlistTaskPatch(status, steps);
      let id = ids.get(host);
      if (patch.kind === "live") {
        if (!id) {
          const name = playlist?.name ?? "";
          id = tasks.registerTask({
            kind: "payload-playlist",
            origin: "payloads.playlist",
            label: name
              ? fill(trStatic("activity_playlist_label", "Payload playlist {name}"), { name })
              : trStatic("activity_playlist_label_unnamed", "Payload playlist"),
            consoleId: host,
          });
          ids.set(host, id);
        }
        tasks.updateTask(id, {
          stage: fill(trStatic("activity_playlist_step", "Step {step} of {total}"), {
            step: patch.step + 1,
            total: steps || patch.step + 1,
          }),
          ...(steps > 0 ? { progress: { current: patch.step, total: steps, unit: "items" as const } } : {}),
        });
        continue;
      }
      if (!id) continue;
      if (patch.kind === "done") tasks.finishTask(id, "done");
      else if (patch.kind === "failed")
        tasks.finishTask(id, "failed", {
          lastError: { code: "PLAYLIST_STEP", message: patch.error, recoverable: true },
        });
      else tasks.finishTask(id, "cancelled");
      ids.delete(host);
    }
  });
}

function installUpdateBridge() {
  let id: string | null = null;
  useUpdateStore.subscribe((s, p) => {
    // The download's bytes as they come in: the row shows its speed and time left (#981).
    if (id && s.downloadProgress && s.downloadProgress !== p.downloadProgress) {
      const { received, total } = s.downloadProgress;
      useTaskStore.getState().updateTask(id, { progress: { current: received, total: total ?? 0, unit: "bytes" } });
    }
    if (s.phase === p.phase) return;
    const phase: UpdatePhase = s.phase;
    const tasks = useTaskStore.getState();
    if (phase.kind === "downloading") {
      if (id) return;
      const version = phase.result.latest_version ?? "";
      id = tasks.registerTask({
        kind: "app-update",
        origin: "app.update",
        label: version
          ? fill(trStatic("activity_update_label", "Download PS5Upload {version}"), { version })
          : trStatic("activity_update_label_plain", "Download the app update"),
        consoleId: "",
      });
      return;
    }
    if (!id) return;
    if (phase.kind === "downloaded") tasks.finishTask(id, "done");
    else if (phase.kind === "download-failed")
      tasks.finishTask(id, "failed", {
        lastError: { code: "UPDATE_DOWNLOAD", message: phase.message, recoverable: true },
      });
    else tasks.finishTask(id, "cancelled");
    id = null;
  });
}

let installed = false;

export function installTaskBridges(): void {
  if (installed) return;
  installed = true;
  installHelperSendBridge();
  installPlaylistBridge();
  installUpdateBridge();
}
