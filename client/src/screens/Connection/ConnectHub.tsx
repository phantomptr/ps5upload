import type { ReactNode } from "react";
import { Cable, MonitorSmartphone, Stethoscope } from "lucide-react";
import { useSearchParams } from "react-router";

import { Tabs } from "../../components";
import { useTr } from "../../state/lang";

export type ConnectTab = "console" | "devices" | "health";

export function connectTabOf(param: string | null): ConnectTab {
  return param === "devices" || param === "health" ? param : "console";
}

/**
 * Connection: everything about reaching the console, as three views of one screen (`?tab=`):
 * connecting and sending the helper, the devices that use the console, and checking the link.
 * Devices and Health Check were separate sidebar entries that all read as "set up my console";
 * their old /devices and /health links land on their tab here.
 */
export function ConnectHub({
  console: consoleView,
  devices,
  health,
}: {
  console: ReactNode;
  devices: ReactNode;
  health: ReactNode;
}) {
  const tr = useTr();
  const [searchParams, setSearchParams] = useSearchParams();
  const tab = connectTabOf(searchParams.get("tab"));
  const show = (next: string) => {
    if (next === tab) return;
    const params = new URLSearchParams(searchParams);
    if (next === "console") params.delete("tab");
    else params.set("tab", next);
    setSearchParams(params);
  };
  const label = tr("connect", undefined, "Connection");
  return (
    <>
      <div className="app-page-tabs">
        <Tabs
          ariaLabel={label}
          value={tab}
          onChange={show}
          tabs={[
            {
              id: "console",
              icon: Cable,
              label: tr("cs_tab_consoles", undefined, "Consoles"),
            },
            {
              id: "devices",
              icon: MonitorSmartphone,
              label: tr("mesh_devices", undefined, "Devices"),
            },
            {
              id: "health",
              icon: Stethoscope,
              label: tr("health", undefined, "Health Check"),
            },
          ]}
        />
      </div>
      <div role="tabpanel" aria-label={label}>
        {tab === "devices" ? devices : tab === "health" ? health : consoleView}
      </div>
    </>
  );
}
