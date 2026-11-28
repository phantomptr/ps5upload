import { barsOf, msText, tierLabel, type Tier } from "../join/routeTier";
import { useTr } from "../state/lang";

/** Colours of the one tier table (design 6.1): green, green, amber, red; grey for nothing yet. */
const COLOR: Record<Tier | "none", string> = {
  excellent: "var(--color-good, #16a34a)",
  good: "var(--color-good, #16a34a)",
  playable: "var(--color-warn, #d97706)",
  poor: "var(--color-bad, #dc2626)",
  none: "var(--color-muted, #888)",
};

/**
 * A connection's quality the same way everywhere (design 6.1): four bars, the word and the round
 * trip in ms. Colour is never the only signal (bar count + word). `estimate`: hollow bars and
 * "about" (a card before joining); `measured`: says so. The word shows on hover/focus when `compact`.
 */
export function PingBars({
  tier,
  ms,
  estimate = false,
  measured = false,
  lastTime = false,
  compact = false,
  detail,
  testId = "ping-bars",
  className = "",
}: {
  tier: Tier | null;
  ms: number | null | undefined;
  /** An estimate (no traffic yet): hollow bars, "about N ms". */
  estimate?: boolean;
  /** A real measurement before joining: "N ms measured". */
  measured?: boolean;
  /** From your own history with this host: "last time N ms". */
  lastTime?: boolean;
  /** Bars + ms only; the word in the tooltip. */
  compact?: boolean;
  /** More on hover (fell behind…). */
  detail?: string;
  testId?: string;
  className?: string;
}) {
  const tr = useTr();
  const n = barsOf(tier);
  const color = COLOR[tier ?? "none"];
  const ms_ = msText(ms);
  const word = tier ? tierLabel(tr, tier) : null;
  const num = ms_
    ? estimate
      ? tr("room_ping_about", { ms: ms_ }, `about ${ms_}`)
      : lastTime
        ? tr("room_ping_last_time", { ms: ms_ }, `last time ${ms_}`)
        : measured
          ? tr("room_ping_measured", { ms: ms_ }, `${ms_} measured`)
          : ms_
    : "—";
  const title = [word, ms_, detail].filter(Boolean).join(" · ") || tr("room_ping_none", undefined, "No reading yet");
  return (
    <span
      className={`inline-flex items-center gap-1.5 whitespace-nowrap text-xs tabular-nums ${className}`}
      title={title}
      data-testid={testId}
      data-tier={tier ?? ""}
      data-bars={n}
      data-kind={estimate ? "estimate" : measured ? "measured" : lastTime ? "last_time" : "live"}
    >
      <span className="inline-flex items-end gap-[2px]" aria-hidden>
        {[1, 2, 3, 4].map((i) => (
          <span
            key={i}
            className="inline-block w-[3px] rounded-[1px]"
            style={{
              height: `${3 + i * 2.5}px`,
              background: i <= n && !estimate ? color : "transparent",
              border: `1px solid ${i <= n ? color : "var(--color-border, #666)"}`,
              opacity: i <= n ? 1 : 0.5,
            }}
          />
        ))}
      </span>
      {!compact && word && <span className="sr-only sm:not-sr-only">{word}</span>}
      <span>{num}</span>
    </span>
  );
}
