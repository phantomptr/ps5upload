import { useState } from "react";
import { useNavigate } from "react-router";
import { Globe, SlidersHorizontal } from "lucide-react";

import { Button, Modal } from "../../components";
import { useTr } from "../../state/lang";
import { selfTailnet, type ConsoleOnTailnet } from "../../state/useTailnet";
import type { AppTailnetStatus } from "../../api/tailnet";
import { BitrateSetting, EnhanceSetting, ResFpsSetting, RumbleSetting, SoundSetting } from "../Play/Settings";
import { SettingsDrawer } from "../Play/SettingsDrawer";

/**
 * TV mode's Play settings (#976): the few a TV needs, one press from the console card. Each is
 * the same control as in Play's own settings (screens/Play/Settings.tsx), on the same saved
 * settings; All Play settings opens Play's full panel.
 */
export function TvPlaySettings({
  open,
  onClose,
  running,
  tailnet,
  consoleTailnet,
}: {
  open: boolean;
  onClose: () => void;
  /** A stream is on: what applies only on the next start says so. */
  running: boolean;
  /** This device's Tailscale (the engine's), or null without one. */
  tailnet: AppTailnetStatus | null;
  /** The console on Tailscale, or null. */
  consoleTailnet: ConsoleOnTailnet | null;
}) {
  const tr = useTr();
  const navigate = useNavigate();
  const [all, setAll] = useState(false);
  const self = selfTailnet(tailnet);
  const away =
    self === "on" || self === "system"
      ? consoleTailnet
        ? tr("tv_away_ready", undefined, "Tailscale is on, and your PS5 is on it: Play works away from home too.")
        : tr("tv_away_no_console", undefined, "Tailscale is on here. Put your PS5 on it too to play away from home.")
      : tr("tv_away_off", undefined, "To play away from home, turn on Tailscale on this device and on your PS5.");
  return (
    <>
      <Modal
        open={open && !all}
        onClose={onClose}
        title={tr("tv_settings_title", undefined, "Play settings")}
        titleIcon={<SlidersHorizontal size={18} aria-hidden />}
        size="lg"
      >
        <div className="space-y-6" data-testid="tv-settings">
          <ResFpsSetting running={running} />
          <BitrateSetting />
          <EnhanceSetting />
          <SoundSetting />
          <RumbleSetting />
          <section className="space-y-2" data-testid="tv-settings-away">
            <h3 className="flex items-center gap-2 text-sm font-semibold">
              <Globe size={16} aria-hidden />
              {tr("tv_away_title", undefined, "Away from home")}
            </h3>
            <p className="text-sm text-[var(--color-muted)]">{away}</p>
            <Button
              size="md"
              onClick={() => {
                onClose();
                navigate("/settings#tailscale");
              }}
              data-testid="tv-settings-tailscale"
            >
              {tr("tv_tailscale_settings", undefined, "Tailscale settings")}
            </Button>
          </section>
          <Button size="md" variant="secondary" onClick={() => setAll(true)} data-testid="tv-settings-all">
            {tr("tv_settings_all", undefined, "All Play settings")}
          </Button>
        </div>
      </Modal>
      <SettingsDrawer
        open={open && all}
        onClose={() => setAll(false)}
        running={running}
        layout="side"
        contained={false}
        underBar={false}
        backdrop
      />
    </>
  );
}
