import { useState } from "react";
import { Gauge, Square } from "lucide-react";

import { Button, Card } from "../../components";
import { ProgressBar } from "../../components/ProgressBar";
import { formatBytes } from "../../lib/format";
import { speedVerdict } from "../../lib/speedVerdict";
import { useTr } from "../../state/lang";
import {
  speedTestFor,
  speedTestRunning,
  stopSpeedTest,
  useSpeedTestStore,
} from "../../state/speedTest";
import { startSpeedTest } from "../../state/speedTestRuntime";

const SIZES = [64, 256, 1024] as const;

const perSec = (bps: number) => `${formatBytes(bps)}/s`;

/** A speed test between this computer and the console: a real file sent with the ordinary
 *  upload route and read back, so the figures are what a copy gets. The run lives in a store
 *  (state/speedTest): it carries on, and keeps its result, when the screen is left. */
export function SpeedTestCard({ host }: { host: string }) {
  const tr = useTr();
  const st = useSpeedTestStore((s) => speedTestFor(s, host));
  const [sizeMib, setSizeMib] = useState<number>(256);
  const running = speedTestRunning(st);
  const verdict =
    st?.phase === "done" ? speedVerdict(st.uploadBps, st.downloadBps) : null;

  const phaseText =
    st?.phase === "preparing"
      ? tr(
          "speedtest.preparing",
          undefined,
          "Making the test file on this computer…",
        )
      : st?.phase === "uploading"
        ? tr("speedtest.uploading", undefined, "Sending to the PS5…")
        : st?.phase === "downloading"
          ? tr(
              "speedtest.downloading",
              undefined,
              "Reading it back from the PS5…",
            )
          : st?.phase === "cleaning"
            ? tr("speedtest.cleaning", undefined, "Removing the test file…")
            : null;
  const verdictText =
    verdict === "gigabit"
      ? tr(
          "speedtest.verdict.gigabit",
          undefined,
          "That is a full gigabit link: as fast as the PS5's network port goes.",
        )
      : verdict === "fast_wifi_or_busy"
        ? tr(
            "speedtest.verdict.mid",
            undefined,
            "Good, but below what a gigabit cable gives (90–115 MB/s). Usual causes: Wi-Fi on either side, or other traffic on the network.",
          )
        : verdict === "hundred_mbit"
          ? tr(
              "speedtest.verdict.hundred",
              undefined,
              "This looks like a 100 Mbit link, which tops out near 11 MB/s. Check the cable (use Cat5e or better) and that every switch or router port in the path is gigabit.",
            )
          : verdict === "slow"
            ? tr(
                "speedtest.verdict.slow",
                undefined,
                "Slow. A weak Wi-Fi signal, a VPN or a busy network is the usual cause. A network cable to the PS5 is the fix.",
              )
            : null;

  return (
    <div className="mb-5" data-testid="speed-test-card">
      <Card className="p-4">
        <div className="flex flex-wrap items-center gap-3">
          <Gauge
            size={18}
            aria-hidden
            className="shrink-0 text-[var(--color-muted)]"
          />
          <div className="min-w-0 flex-1">
            <div className="text-sm font-semibold">
              {tr("speedtest.title", undefined, "Speed test")}
            </div>
            <div className="text-xs text-[var(--color-muted)]">
              {tr(
                "speedtest.help",
                undefined,
                "Sends a test file to the PS5 and reads it back, the same way a real copy goes, then removes it from both. It measures your network between this computer and the PS5, not your internet.",
              )}
            </div>
          </div>
          <label className="flex items-center gap-2 text-xs text-[var(--color-muted)]">
            {tr("speedtest.size", undefined, "Test size")}
            <select
              value={sizeMib}
              disabled={running}
              onChange={(e) => setSizeMib(Number(e.currentTarget.value))}
              className="rounded border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 text-sm text-[var(--color-text)]"
            >
              {SIZES.map((s) => (
                <option key={s} value={s}>
                  {formatBytes(s * 1024 * 1024)}
                </option>
              ))}
            </select>
          </label>
          {running ? (
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<Square size={12} />}
              onClick={() => stopSpeedTest(host)}
            >
              {tr("stop", undefined, "Stop")}
            </Button>
          ) : (
            <Button
              variant="primary"
              size="sm"
              onClick={() => void startSpeedTest(host, sizeMib)}
              data-testid="speed-test-start"
            >
              {st
                ? tr("speedtest.again", undefined, "Test again")
                : tr("speedtest.start", undefined, "Start test")}
            </Button>
          )}
        </div>

        {running && phaseText && (
          <div className="mt-3" role="status">
            <div className="mb-1 flex items-center justify-between text-xs">
              <span>{phaseText}</span>
              {st && st.total > 0 && (
                <span className="tabular-nums text-[var(--color-muted)]">
                  {formatBytes(st.sent)} / {formatBytes(st.total)}
                </span>
              )}
            </div>
            <ProgressBar
              value={st && st.total > 0 ? st.sent / st.total : null}
              size="sm"
            />
          </div>
        )}

        {st && (st.uploadBps !== null || st.downloadBps !== null) && (
          <dl
            className="mt-3 grid gap-3 sm:grid-cols-2"
            data-testid="speed-test-result"
          >
            <div className="rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface)] p-3">
              <dt className="text-xs text-[var(--color-muted)]">
                {tr("speedtest.up", undefined, "To the PS5 (upload)")}
              </dt>
              <dd className="text-lg font-semibold tabular-nums">
                {st.uploadBps !== null ? perSec(st.uploadBps) : "—"}
              </dd>
            </div>
            <div className="rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface)] p-3">
              <dt className="text-xs text-[var(--color-muted)]">
                {tr("speedtest.down", undefined, "From the PS5 (download)")}
              </dt>
              <dd className="text-lg font-semibold tabular-nums">
                {st.downloadBps !== null ? perSec(st.downloadBps) : "—"}
              </dd>
            </div>
          </dl>
        )}
        {verdictText && (
          <p className="mt-2 text-xs text-[var(--color-muted)]">
            {verdictText}
          </p>
        )}
        {st?.phase === "stopped" && (
          <p className="mt-2 text-xs text-[var(--color-muted)]">
            {tr(
              "speedtest.stopped",
              undefined,
              "Stopped. The test file was removed.",
            )}
          </p>
        )}
        {st?.phase === "failed" && (
          <p role="alert" className="mt-2 text-xs text-[var(--color-bad)]">
            {st.failedLeg === "prepare"
              ? tr(
                  "speedtest.failed.prepare",
                  { error: st.error ?? "" },
                  "Could not make the test file on this computer: {error}",
                )
              : st.failedLeg === "upload"
                ? tr(
                    "speedtest.failed.upload",
                    { error: st.error ?? "" },
                    "Sending to the PS5 failed: {error}",
                  )
                : tr(
                    "speedtest.failed.download",
                    { error: st.error ?? "" },
                    "Reading back from the PS5 failed: {error}",
                  )}
          </p>
        )}
      </Card>
    </div>
  );
}
