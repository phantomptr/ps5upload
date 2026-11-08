import { Select, Toggle } from "../../components";
import { bringUpPlaylistIdFor } from "../../lib/playlistOps";
import { setBringUpPlaylistFor, setConsoleSetting, useConsoleSettings } from "../../state/consoleSettings";
import { useTr } from "../../state/lang";
import { usePayloadPlaylistsStore } from "../../state/payloadPlaylists";

/**
 * "Bring up automatically" and the bring-up playlist, for one console (by roster profile id).
 * Settings › This PS5, Connection › Quick bring-up and Payloads › Playlists all show these same
 * controls over the same per-console values, so the three places can never disagree.
 */
export function AutoBringUpToggle({
  consoleId,
  hint,
  className,
}: {
  consoleId: string | null;
  /** The longer explanation (Settings) or the short one (Connection). */
  hint: string;
  className?: string;
}) {
  const tr = useTr();
  const on = useConsoleSettings(consoleId).auto_bring_up;
  return (
    <Toggle
      className={className}
      checked={on}
      disabled={!consoleId}
      onChange={(checked) => consoleId && setConsoleSetting(consoleId, "auto_bring_up", checked)}
      label={tr("auto_bringup_label", undefined, "Bring up automatically")}
      hint={hint}
    />
  );
}

/** Reactive: the bring-up playlist id of a console, or the default. */
export function useBringUpPlaylistId(consoleId: string | null): string | null {
  return usePayloadPlaylistsStore((s) => bringUpPlaylistIdFor(s.autoLoader, consoleId));
}

/** The bring-up playlist picker for one console. `compact`: the inline form of Payloads. */
export function BringUpPlaylistSelect({
  consoleId,
  compact = false,
}: {
  consoleId: string | null;
  compact?: boolean;
}) {
  const tr = useTr();
  const playlists = usePayloadPlaylistsStore((s) => s.playlists);
  const value = useBringUpPlaylistId(consoleId);
  const label = tr("bringup_playlist_label_ps5", undefined, "Bring-up playlist for this PS5 (kernel R/W, SMP)");
  return (
    <Select
      label={compact ? undefined : label}
      aria-label={compact ? label : undefined}
      hint={
        compact
          ? undefined
          : tr(
              "bringup_playlist_hint_ps5",
              undefined,
              "Runs before the helper during Quick bring-up. Payloads depend on the firmware, so each PS5 has its own. Make playlists in Payloads.",
            )
      }
      value={value ?? ""}
      disabled={!consoleId}
      onChange={(e) => consoleId && setBringUpPlaylistFor(consoleId, e.target.value || null)}
      block={!compact}
      className={compact ? "min-w-[min(100%,10rem)] text-xs" : "text-xs"}
      data-testid="bringup-playlist-select"
    >
      <option value="">{tr("autoloader_bringup_none", undefined, "None (send helper only)")}</option>
      {playlists.map((p) => (
        <option key={p.id} value={p.id}>
          {p.name}
        </option>
      ))}
    </Select>
  );
}
