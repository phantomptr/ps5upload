import { useEffect, useState } from "react";
import { ExternalLink, Trash2 } from "lucide-react";
import { Button, Toggle } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import {
  minionSet,
  type MinionChange,
  type MinionTileState,
} from "../../api/consoleFiles";
import { hostOf } from "../../lib/addr";
import { openExternalUrl } from "../../lib/openExternalUrl";
import { useConnectionStore } from "../../state/connection";
import { useTr } from "../../state/lang";
import {
  markMinionHandled,
  minionOffForNew,
  rememberMinion,
  setMinionOffForNew,
} from "../../state/minionMemory";
import { useRosterStore } from "../../state/roster";
import { consoleOnTailnet, useTailnet } from "../../state/useTailnet";

type Tr = ReturnType<typeof useTr>;

/** Settings › This PS5: Minion on one console (on/off, home-screen tile, LAN page). */
export function MinionConsoleSettings({ host }: { host: string }) {
  const up = useConnectionStore((s) => s.runtimeByHost[hostOf(host.trim())]?.payloadStatus === "up");
  return <MinionTileControl key={`${host}:${up}`} host={host} up={up} />;
}

/** Settings › App: this computer's choices for every console: Minion off on consoles added from
 *  now on (#715). */
export function MinionDefaults() {
  const tr = useTr();
  const [offForNew, setOffForNew] = useState(minionOffForNew);
  return (
    <div className="space-y-2 text-sm">
      <Toggle
        checked={offForNew}
        onChange={(on) => {
          const known = useRosterStore
            .getState()
            .profiles.map((p) => (p.host ?? "").trim())
            .filter(Boolean);
          setMinionOffForNew(on, known);
          setOffForNew(on);
        }}
        label={tr("minion_off_for_new_label", undefined, "Keep Minion off on consoles I add")}
        hint={tr(
          "minion_off_for_new_hint",
          undefined,
          "When a console connects to this app for the first time, Minion is turned off on it once. Consoles you already have stay as they are, and turning Minion back on for one console keeps it on.",
        )}
      />
      <p className="text-xs text-[var(--color-muted)]">
        {tr(
          "minion_boot_helper_note",
          undefined,
          "Minion needs the helper running. To start it with the PS5, turn on Payloads → Payload Manager → Load the PS5Upload helper at boot.",
        )}
      </p>
    </div>
  );
}

/** The line under the controls: what the helper last did, in words. */
export function minionStatus(
  state: Extract<MinionTileState, { supported: true }>,
  tr: Tr,
): string {
  if (state.tile_busy)
    return tr("cw_files_perm_applying", undefined, "Updating the home screen…");
  if (state.enabled === false) {
    if (state.tile_failed)
      return tr(
        "minion_off_failed",
        undefined,
        "Minion is off, but its home-screen tile could not be removed. Turn Minion on and off again to retry.",
      );
    if (state.tile_result === "hidden")
      return tr(
        "minion_off_hidden",
        undefined,
        "Minion is off. The PS5 kept its record of the tile, so its icon is hidden instead.",
      );
    return tr(
      "minion_off_removed",
      undefined,
      "Minion is off: its home-screen tile is removed and the page on the console is stopped.",
    );
  }
  // Also after a failed add at helper start, before anyone pressed a button: says why.
  if (state.tile_failed && state.tile && state.tile_error)
    return tr(
      "cw_settings_tile_failed_reason",
      { reason: state.tile_error },
      "Could not add PS5Upload Minion to the home screen ({reason}). Try again.",
    );
  if (state.tile_failed)
    return tr(
      state.tile ? "cw_settings_err_tile_add" : "cw_settings_err_tile_remove",
      undefined,
      "The home screen update failed. Try again.",
    );
  if (state.tile)
    return tr(
      "minion_tile_on_state",
      undefined,
      "The home-screen tile is shown.",
    );
  if (state.tile_result === "hidden")
    return tr(
      "cw_settings_tile_hidden",
      undefined,
      "The PS5 kept its record of PS5Upload Minion, so its home-screen icon is hidden instead.",
    );
  return tr(
    "minion_tile_off_state",
    undefined,
    "The home-screen tile is hidden.",
  );
}

/** The address other devices open Minion at, with network access on. */
export function minionUrl(host: string, port?: number): string {
  const h = hostOf(host);
  return `http://${h.includes(":") ? `[${h}]` : h}:${port ?? 9180}/`;
}

export interface MinionViewProps {
  up: boolean;
  state: MinionTileState | null;
  busy: boolean;
  onApply: (change: MinionChange) => void;
  /** The console's address (for the network link). */
  host?: string;
  /** Remove was pressed (the caller asks first). */
  onRemove?: () => void;
}

/** Settings → Minion without state (tests render it directly). */
/** Reached through Tailscale now: the console's page is not carried there (only Remote Play and
 *  PS5Upload's own connection are), so its link says so instead of a page that never loads. */
function MinionTailnetNote({ host }: { host: string }) {
  const tr = useTr();
  const { status } = useTailnet(!!host);
  if ((consoleOnTailnet(status, host)?.via ?? "lan") === "lan") return null;
  return (
    <p className="w-full text-xs text-[var(--color-muted)]" data-testid="minion-tailnet-note">
      {tr(
        "minion_tailnet_note",
        undefined,
        "This PS5 is reached through Tailscale right now, which doesn't carry this page. It opens on the PS5's home network.",
      )}
    </p>
  );
}

export function MinionView({
  up,
  state,
  busy,
  onApply,
  host = "",
  onRemove,
}: MinionViewProps) {
  const tr = useTr();
  const ready = state?.supported ? state : null;
  // An older helper has no on/off switch: only the tile buttons, as before.
  const canSwitch = typeof ready?.enabled === "boolean";
  const on = ready?.enabled !== false;
  const locked = busy || !!ready?.tile_busy;
  // Helpers from 7.3.1 report `saved` and take lan / remove from here.
  const canManage = typeof ready?.saved === "boolean";
  return (
    <>
      {!up && (
        <p>
          {tr(
            "minion_tile_connect",
            undefined,
            "Connect to the helper to change Minion visibility.",
          )}
        </p>
      )}
      {state?.supported === false && (
        <p>
          {tr(
            "minion_tile_update",
            undefined,
            "Update the helper to use this control.",
          )}
        </p>
      )}
      {ready && (
        <>
          {canSwitch ? (
            <Toggle
              checked={on}
              disabled={locked}
              onChange={(enabled) => onApply({ enabled })}
              label={`${tr("cw_settings_app_name", undefined, "PS5Upload Minion")}: ${
                on
                  ? tr("cw_settings_on", undefined, "On")
                  : tr("cw_settings_off", undefined, "Off")
              }`}
              hint={tr(
                "minion_enabled_note",
                undefined,
                "Off removes the home-screen tile and stops the page on the console. The PC app keeps working, and you can turn Minion back on here at any time.",
              )}
            />
          ) : (
            <p className="text-[var(--color-muted)]">
              {tr(
                "minion_enabled_update",
                undefined,
                "Update the helper to turn PS5Upload Minion off completely.",
              )}
            </p>
          )}
          {on && (
            <p>
              {tr(
                "cw_settings_tile",
                undefined,
                "Show or hide Minion on the PS5 home screen using the paired helper connection.",
              )}
            </p>
          )}
          <p role="status">{minionStatus(ready, tr)}</p>
          {typeof ready.removed === "number" && (
            <p className="text-[var(--color-good)]">
              {tr(
                "minion_removed_done",
                { n: ready.removed },
                "Minion is removed: its tile and page are gone and {n} of its files on the PS5 were deleted. It stays off, even after the helper restarts, until you turn it on here.",
              )}
            </p>
          )}
          {on && (
            <div className="flex flex-wrap gap-2">
              <Button
                size="sm"
                disabled={locked}
                onClick={() => onApply({ tile: true })}
              >
                {tr("minion_tile_show", undefined, "Show / restore Minion")}
              </Button>
              <Button
                size="sm"
                variant="secondary"
                disabled={locked}
                onClick={() => onApply({ tile: false })}
              >
                {tr("minion_tile_hide", undefined, "Hide Minion")}
              </Button>
            </div>
          )}
          {on && typeof ready.lan === "boolean" && (
            <div className="space-y-2 border-t border-[var(--glass-edge)] pt-3">
              <Toggle
                checked={ready.lan}
                disabled={locked}
                onChange={(lan) => onApply({ lan })}
                label={tr(
                  "minion_lan_label",
                  undefined,
                  "Open Minion from other devices on your network",
                )}
                hint={tr(
                  "minion_lan_hint",
                  undefined,
                  "Off: only the PS5's own browser opens it. On: a phone, tablet or computer on the same network can open it too, so anyone on that network can. Turn it on at home only.",
                )}
              />
              {ready.lan && host && (
                <div className="flex flex-wrap items-center gap-2">
                  <code className="rounded bg-[var(--color-surface-3)] px-2 py-1 text-xs">
                    {minionUrl(host, ready.port)}
                  </code>
                  <Button
                    size="sm"
                    variant="secondary"
                    leftIcon={<ExternalLink size={13} />}
                    onClick={() => void openExternalUrl(minionUrl(host, ready.port))}
                  >
                    {tr("minion_open_here", undefined, "Open here")}
                  </Button>
                  <MinionTailnetNote host={host} />
                </div>
              )}
            </div>
          )}
          {canManage && (
            <div className="space-y-2 border-t border-[var(--glass-edge)] pt-3">
              <p className="text-[var(--color-muted)]">
                {tr(
                  "minion_remove_note",
                  undefined,
                  "Remove turns Minion off and deletes what it keeps on the PS5: its Files favourites and recent folders, its game Library cache and its controller log. Network shares, cheats and backups stay; the PC app uses them too.",
                )}
              </p>
              <Button
                size="sm"
                variant="danger"
                leftIcon={<Trash2 size={13} />}
                disabled={locked}
                onClick={() => onRemove?.()}
              >
                {tr("minion_remove", undefined, "Remove Minion from this PS5…")}
              </Button>
            </div>
          )}
        </>
      )}
    </>
  );
}

function MinionTileControl({ host, up }: { host: string; up: boolean }) {
  const tr = useTr();
  const [state, setState] = useState<MinionTileState | null>(null);
  const [busy, setBusy] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (!host || !up || busy) return;
    let gone = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const poll = async () => {
      try {
        const s = await minionSet(host);
        if (gone) return;
        // A Remove's "n files deleted" stays up while Minion stays off.
        setState((prev) =>
          prev?.supported &&
          typeof prev.removed === "number" &&
          s.supported &&
          s.enabled === false
            ? { ...s, removed: prev.removed }
            : s,
        );
        if (s.supported && s.tile_busy)
          timer = setTimeout(() => void poll(), 1500);
      } catch (e) {
        if (!gone) setError(String(e));
      }
    };
    void poll();
    return () => {
      gone = true;
      clearTimeout(timer);
    };
  }, [host, up, busy, attempt]);
  const { confirm, dialog } = useConfirm();
  const apply = async (change: MinionChange) => {
    setBusy(true);
    setError(null);
    try {
      const next = await minionSet(host, change);
      setState(next);
      // Remembered here so a console that loses its settings is turned off again (minionMemory).
      if (next.supported && change.remove) rememberMinion(host, true);
      else if (next.supported && change.enabled !== undefined)
        rememberMinion(host, !change.enabled);
      // Chosen by hand: "Keep Minion off on consoles I add" never changes it again.
      if (next.supported && (change.remove || change.enabled !== undefined))
        markMinionHandled(host);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="space-y-3 text-sm">
      <MinionView
        up={up}
        state={state}
        busy={busy}
        host={host}
        onApply={(c) => void apply(c)}
        onRemove={() =>
          void confirm({
            title: tr("minion_remove_title", undefined, "Remove PS5Upload Minion?"),
            message: tr(
              "minion_remove_confirm",
              undefined,
              "Its home-screen tile and page go, and its own files on the PS5 are deleted. The helper keeps it off from now on. You can turn it back on here at any time.",
            ),
            confirmLabel: tr("minion_remove_go", undefined, "Remove"),
            destructive: true,
          }).then((ok) => {
            if (ok) void apply({ remove: true });
          })
        }
      />
      {dialog}
      {error && (
        <div>
          <p role="alert" className="text-[var(--color-bad)]">
            {error}
          </p>
          <Button
            size="sm"
            disabled={busy}
            onClick={() => {
              setError(null);
              setAttempt((n) => n + 1);
            }}
          >
            {tr("retry", undefined, "Retry")}
          </Button>
        </div>
      )}
    </div>
  );
}
