import { ArrowRight, Cable, Zap } from "lucide-react";
import { useNavigate } from "react-router";

import { Button, Callout, Select, Toggle } from "../../components";
import { isMobile } from "../../lib/platform";
import { isTauriEnv } from "../../lib/tauriEnv";
import { setConsoleSetting, useConsoleSettings } from "../../state/consoleSettings";
import { useTr } from "../../state/lang";
import { useRosterStore, type PS5Profile } from "../../state/roster";
import type { KeepPs5AwakeMode } from "../../state/uploadSettings";
import TailnetPanel from "../Connection/TailnetPanel";
import { payloadCardLink } from "../Payloads/payloadTabs";
import { AutoBringUpToggle, BringUpPlaylistSelect } from "./ConsoleBringUp";
import { MinionConsoleSettings } from "./MinionSettings";
import { PlayTogetherGuestProfiles } from "./PlayTogetherGuestProfiles";
import { GroupHeading, Section } from "./SettingsLayout";
import { settingsConsoleId } from "./settingsTabs";
import { settingsVisibility } from "./visibility";

/**
 * Settings › This PS5: one console's own settings, with a picker that starts on the selected
 * console (or the one `?console=` names). Every switch here writes that console's value
 * (state/consoleSettings), whichever console is on screen elsewhere in the app.
 */
export function Ps5SettingsTab({
  requested,
  onPick,
}: {
  /** The roster profile id the address asks for (`?console=`), if any. */
  requested: string | null;
  onPick: (id: string) => void;
}) {
  const tr = useTr();
  const navigate = useNavigate();
  const profiles = useRosterStore((s) => s.profiles);
  const activeId = useRosterStore((s) => s.active_id);
  const id = settingsConsoleId(requested, profiles, activeId);
  const profile = profiles.find((p) => p.id === id) ?? null;

  if (!profile) {
    return (
      <Callout tone="info" title={tr("settings_ps5_none_title", undefined, "No PS5 yet")}>
        <p>
          {tr(
            "settings_ps5_none_body",
            undefined,
            "Add your PS5 on the Connection screen, then its own settings are here.",
          )}
        </p>
        <Button
          className="mt-2"
          size="sm"
          variant="secondary"
          leftIcon={<Cable size={14} />}
          onClick={() => navigate("/connection")}
        >
          {tr("settings_ps5_open_connection", undefined, "Open Connection")}
        </Button>
      </Callout>
    );
  }

  return (
    <div className="grid w-full gap-4 md:grid-cols-2" data-testid="settings-ps5">
      <div className="md:col-span-2">
        <Select
          label={tr("settings_ps5_picker", undefined, "PS5")}
          value={profile.id}
          onChange={(e) => onPick(e.target.value)}
          options={profiles.map((p) => ({ value: p.id, label: consoleLabel(p) }))}
          hint={tr(
            "settings_ps5_picker_hint",
            undefined,
            "These settings belong to this PS5 only. Each of your consoles keeps its own, and they follow it to your other devices.",
          )}
          data-testid="settings-ps5-picker"
          className="md:max-w-md"
        />
      </div>
      {/* Keyed by console: per-console panels reload their state for the one picked. */}
      <Ps5SettingsBody key={profile.id} profile={profile} />
    </div>
  );
}

function consoleLabel(p: PS5Profile): string {
  const name = p.name?.trim();
  const host = p.host?.trim();
  return name && host && !name.includes(host) ? `${name} (${host})` : name || host || p.id;
}

function Ps5SettingsBody({ profile }: { profile: PS5Profile }) {
  const tr = useTr();
  const navigate = useNavigate();
  const id = profile.id;
  const host = profile.host.trim();
  const s = useConsoleSettings(id);
  const show = settingsVisibility({ tauri: isTauriEnv(), mobile: isMobile() });
  // The other screens act on the selected console: select this one first.
  const openFor = (to: string) => {
    if (useRosterStore.getState().active_id !== id) useRosterStore.getState().setActive(id);
    navigate(to);
  };

  return (
    <>
      <GroupHeading>{tr("settings_group_general", undefined, "General")}</GroupHeading>

      <Section title={tr("cw_settings_app_name", undefined, "PS5Upload Minion")} full>
        <MinionConsoleSettings host={host} />
      </Section>

      <GroupHeading>{tr("settings_group_console", undefined, "Console")}</GroupHeading>

      {/* Bring-up sends payloads from this machine: the desktop and phone apps only. */}
      {show.reconnectAfterRest && (
        <Section title={tr("settings_card_console_connect", undefined, "Connecting")} full>
          <div className="grid gap-3">
            <AutoBringUpToggle
              consoleId={id}
              hint={tr(
                "auto_bringup_hint_ps5",
                undefined,
                "When this PS5's loader (elfldr, port 9021) is up and the helper isn't running, the app runs Quick bring-up by itself: this PS5's bring-up playlist if you set one, then the helper, then your auto-loader playlist. Once each time this PS5 starts; after rest mode only the helper is sent again. It waits about 90 seconds after the loader appears, so the console's own autoloader can finish first. Off by default.",
              )}
            />
            <BringUpPlaylistSelect consoleId={id} />
          </div>
        </Section>
      )}

      <Section title={tr("settings_card_console_rest", undefined, "Rest mode")} full>
        <div className="grid gap-3">
          {show.reconnectAfterRest && (
            <Toggle
              checked={s.auto_redeploy_on_wake}
              onChange={(on) => setConsoleSetting(id, "auto_redeploy_on_wake", on)}
              label={tr(
                "upload_auto_redeploy_on_wake",
                undefined,
                "Reconnect automatically after rest mode / network drops",
              )}
              hint={tr(
                "upload_auto_redeploy_on_wake_hint_ps5",
                undefined,
                "When this PS5's helper goes offline (rest mode, a Wi-Fi switch, or a payload crash), keep trying to send it again in the background, so the helper, this PS5's fan threshold and the upload port come back by themselves once it is reachable again. On by default.",
              )}
            />
          )}

          <div className="text-sm">
            <div className="flex items-center gap-2 font-medium">
              <Zap size={14} />
              {tr("keep_ps5_awake_mode", "Keep the PS5 awake")}
            </div>
            <Select
              id={`keep-ps5-awake-mode-${id}`}
              aria-label={tr("keep_ps5_awake_mode", "Keep the PS5 awake")}
              value={s.keep_awake_mode}
              onChange={(e) => setConsoleSetting(id, "keep_awake_mode", e.target.value as KeepPs5AwakeMode)}
              options={[
                { value: "off", label: tr("keep_awake_mode_off", "Off") },
                { value: "transfers", label: tr("keep_awake_mode_transfers_v2", "During uploads and installs") },
                { value: "always", label: tr("keep_awake_mode_always", "Always while connected") },
              ]}
              block={false}
              className="mt-1 text-xs"
            />
            <div className="mt-0.5 text-xs text-[var(--color-muted)]">
              {tr(
                "keep_ps5_awake_mode_hint_ps5",
                undefined,
                "Resets this PS5's auto-standby timer so it doesn't go into rest mode. “During uploads and installs” protects long uploads (a common cause of failed uploads; default). “Always while connected” keeps this PS5 out of rest mode while its helper runs and the app is open. Putting the PS5 to rest yourself still works in every mode.",
              )}
            </div>
          </div>

          <Toggle
            checked={s.rest_after_upload}
            onChange={(on) => setConsoleSetting(id, "rest_after_upload", on)}
            label={tr("upload_rest_after", undefined, "Put the PS5 in rest mode after uploads finish")}
            hint={tr(
              "upload_rest_after_hint_ps5",
              undefined,
              "When this PS5's upload queue finishes, ask it to go into rest mode, handy for an overnight queue. Only when at least one upload completed (not after a Stop or an all-failed run). Off by default. Some firmware refuses rest mode; you'll get a notice if this PS5 does.",
            )}
          />
        </div>
      </Section>

      <Section title={tr("pt_button", undefined, "Play Together")} full>
        <PlayTogetherGuestProfiles host={host} />
      </Section>

      {/* "Put this PS5 on Tailscale": the same panel as on the Connection screen. */}
      <div className="md:col-span-2" id="ps5-tailscale">
        <TailnetPanel host={host} />
      </div>

      <GroupHeading>{tr("settings_group_uploads", undefined, "Uploads")}</GroupHeading>

      <Section title={tr("settings_card_upload_behavior", undefined, "Behavior")} full>
        <Toggle
          checked={s.wake_for_uploads}
          onChange={(on) => setConsoleSetting(id, "wake_for_uploads", on)}
          label={tr("upload_wake_for_uploads", undefined, "Wake the PS5 for a waiting upload")}
          hint={tr(
            "upload_wake_for_uploads_hint_ps5",
            undefined,
            "When an upload is waiting for this PS5 in rest mode, send it a wake signal and carry on once its helper is back. Needs wake set up for this PS5 on the Connection screen. On by default.",
          )}
        />
      </Section>

      <GroupHeading>{tr("settings_group_installs", undefined, "Installs")}</GroupHeading>

      <Section
        title={tr("settings_card_install_fallback", undefined, "When the PS5 cannot fetch a package")}
        full
      >
        <Toggle
          checked={s.install_path_fallback}
          onChange={(on) => setConsoleSetting(id, "install_path_fallback", on)}
          label={tr(
            "install_path_fallback_label",
            undefined,
            "Install by file path when the PS5 cannot reach this computer",
          )}
          hint={tr(
            "install_path_fallback_hint_ps5",
            undefined,
            "A last resort for a package that is already on this PS5, when it refuses the package from its own storage and cannot fetch it from this computer either. Only used for a base game that is not installed, so it cannot remove anything. Unproven: it has worked on firmware 11.20 and is refused on 13.60, so it is set for each PS5. If you try it, please report whether it worked.",
          )}
        />
      </Section>

      <GroupHeading>{tr("settings_group_ps5_elsewhere", undefined, "More for this PS5")}</GroupHeading>

      <Section title={tr("settings_card_ps5_elsewhere", undefined, "On other screens")} full>
        <p className="mb-3 text-xs text-[var(--color-muted)]">
          {tr(
            "settings_ps5_elsewhere_hint",
            undefined,
            "These are kept on this PS5 itself and set where they are used. Opening one selects this PS5.",
          )}
        </p>
        <div className="flex flex-wrap gap-2">
          <PsLink onClick={() => openFor("/console")} label={tr("settings_ps5_fan_link", undefined, "Fan threshold")} />
          <PsLink
            onClick={() => openFor(payloadCardLink("payload-manager"))}
            label={tr("settings_ps5_pm_link", undefined, "Payload Manager: what loads at boot")}
          />
          <PsLink
            onClick={() => openFor("/shared-folders")}
            label={tr("settings_ps5_shares_link", undefined, "Shares on this PS5")}
          />
          <PsLink
            onClick={() => openFor("/connection")}
            label={tr("settings_ps5_connection_link", undefined, "Connection and wake")}
          />
        </div>
      </Section>
    </>
  );
}

function PsLink({ onClick, label }: { onClick: () => void; label: string }) {
  return (
    <Button size="sm" variant="secondary" rightIcon={<ArrowRight size={12} className="rtl:rotate-180" />} onClick={onClick}>
      {label}
    </Button>
  );
}
