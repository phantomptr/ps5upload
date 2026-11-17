import { useInRouterContext, Link } from "react-router";

import { DOC_ANCHORS, faqLink } from "../lib/installErrorDoc";
import { LAST_PS5_FAKE_GAME_FIRMWARE, ps5FakeGameUnplayableFirmware } from "../lib/ps5Firmware";
import { useTr } from "../state/lang";
import { Callout } from "./Callout";

/**
 * A fake PS5 game package on firmware above 11.60 installs but does not start: Kraken Mount
 * mounts it instead, and support for installed packages there (the A53 patches) is coming. Renders nothing for a PS4 package, for PS5 homebrew (IV…/ITEM…), for
 * 11.60 and older, or when the firmware can't be read: the warning needs a
 * PS5 game id AND a known firmware above the line.
 */
export function FakeGameFirmwareNotice({
  kernel,
  contentId,
  className = "",
  compact = false,
}: {
  /** The console's kernel string (`ps5Kernel`). */
  kernel: string | null | undefined;
  /** The package's content id or title id. */
  contentId: string | null | undefined;
  className?: string;
  /** One line of text instead of a titled callout, for a list row. */
  compact?: boolean;
}) {
  const tr = useTr();
  const inRouter = useInRouterContext();
  const fw = ps5FakeGameUnplayableFirmware(kernel, contentId);
  if (!fw) return null;
  const body = tr(
    "fakegame.fw.body_v2",
    { fw, last: LAST_PS5_FAKE_GAME_FIRMWARE },
    `PS5 fake packages install on firmware above ${LAST_PS5_FAKE_GAME_FIRMWARE} but don't start. Mount the package with Kraken Mount instead: the game plays straight from the .pkg, nothing installed. Full support for installed packages on newer firmware (through the A53 patches) is coming. PS4 packages and PS5 homebrew apps are fine.`,
  );
  const help = inRouter ? (
    <Link
      to={faqLink(DOC_ANCHORS.wontLaunch)}
      className="ms-1 underline underline-offset-2"
    >
      {tr("fakegame.fw.more", undefined, "Why?")}
    </Link>
  ) : null;
  if (compact) {
    return (
      <p role="status" className={`text-xs text-[var(--color-warn)] ${className}`}>
        {body}
        {help}
      </p>
    );
  }
  return (
    <Callout
      tone="warn"
      className={className}
      title={tr(
        "fakegame.fw.title_v2",
        { fw },
        `Installed, this PS5 game won't start on FW ${fw}`,
      )}
    >
      {body}
      {help}
    </Callout>
  );
}
