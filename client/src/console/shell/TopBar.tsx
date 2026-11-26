// public/logo-square.png at 96 px, 64 colours (1.2 KB, not 6.7 KB): drawn at 1.8rem, at most ~90 px.
import logo from "./logo.png?inline";

/** What /api/status answers (payload/src/webui_api.c r_status). */
export interface ConsoleStatus {
  version: string | null;
  firmware: string | null;
  model: string | null;
  temp_c: number | null;
  running: { title_id: string; app_id: number; name: string | null } | null;
  /** The console's time zone, minutes ahead of UTC (summer time included); null when it cannot
   *  tell, absent from an older helper. The console's own browser has no zone (time.ts). */
  tz_min?: number | null;
}

const BRAND = "PS5Upload Minion";

/** Logo, name, and the console chip: model · firmware · temperature. */
export function TopBar({ status, chipFallback }: { status: ConsoleStatus | null; chipFallback: string | null }) {
  const parts = status
    ? [status.model, status.firmware, status.temp_c != null ? `${status.temp_c}°C` : null].filter(
        (p): p is string => !!p,
      )
    : [];
  const chip = chipFallback ?? parts.join(" · ");
  return (
    <header className="cw-top">
      <img src={logo} alt="" width={40} height={40} />
      <span className="cw-brand">{BRAND}</span>
      {chip ? <span className={`cw-chip${chipFallback ? " is-off" : ""}`}>{chip}</span> : null}
    </header>
  );
}
