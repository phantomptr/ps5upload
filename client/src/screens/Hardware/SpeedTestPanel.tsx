import { useState } from "react";
import { Gauge, Loader2, Play, AlertTriangle } from "lucide-react";
import { netSpeedTestRun, type NetSpeedTestResult } from "../../api/ps5";
import { Button } from "../../components";
import { useTr } from "../../state/lang";

/**
 * Network round-trip latency test.
 *
 * Sends N empty NetSpeedTest frames over the management port and
 * times each ACK. Result is a coarse RTT distribution — useful for
 * "is the LAN actually fast?" diagnostics. Doesn't measure actual
 * upload throughput (the small frame body would saturate the LAN
 * differently than a real shard); for that, run a real upload and
 * watch the speed.
 */
export default function SpeedTestPanel({ mgmtAddr }: { mgmtAddr: string }) {
  const tr = useTr();
  const [result, setResult] = useState<NetSpeedTestResult | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [rounds, setRounds] = useState(64);

  async function run() {
    setBusy(true);
    setError(null);
    setResult(null);
    try {
      const r = await netSpeedTestRun(mgmtAddr, rounds);
      setResult(r);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4">
      <header className="mb-3 flex items-center gap-2">
        <Gauge size={14} />
        <h3 className="flex-1 text-sm font-semibold">
          {tr("speed_test_title", undefined, "Network round-trip test")}
        </h3>
      </header>
      <div className="flex flex-wrap items-center gap-2">
        <label className="text-xs text-[var(--color-muted)]">
          {tr("speed_test_rounds", undefined, "Rounds")}
          <input
            type="number"
            min={4}
            max={2048}
            step={4}
            value={rounds}
            onChange={(e) => {
              const n = parseInt(e.target.value, 10);
              if (!isNaN(n)) setRounds(Math.max(4, Math.min(2048, n)));
            }}
            disabled={busy}
            className="ml-2 w-20 rounded border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 text-xs"
          />
        </label>
        <Button
          variant="primary"
          size="sm"
          leftIcon={
            busy ? (
              <Loader2 size={11} className="animate-spin" />
            ) : (
              <Play size={11} />
            )
          }
          onClick={run}
          disabled={busy}
        >
          {busy
            ? tr("speed_test_running", undefined, "Running…")
            : tr("speed_test_run", undefined, "Run test")}
        </Button>
      </div>
      {error && (
        <div className="mt-2 flex items-start gap-1 text-[11px] text-[var(--color-bad)]">
          <AlertTriangle size={11} className="mt-0.5 shrink-0" />
          {error}
        </div>
      )}
      {result && (
        <dl className="mt-3 grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-xs">
          <dt className="text-[var(--color-muted)]">
            {tr("speed_test_rounds_done", undefined, "Round trips")}
          </dt>
          <dd className="tabular-nums">{result.round_trips}</dd>
          <dt className="text-[var(--color-muted)]">
            {tr("speed_test_total", undefined, "Total time")}
          </dt>
          <dd className="tabular-nums">{result.elapsed_ms} ms</dd>
          <dt className="text-[var(--color-muted)]">
            {tr("speed_test_avg", undefined, "Avg RTT")}
          </dt>
          <dd className="tabular-nums">
            {(result.avg_rtt_us / 1000).toFixed(2)} ms
          </dd>
          <dt className="text-[var(--color-muted)]">
            {tr("speed_test_p50", undefined, "Median (p50)")}
          </dt>
          <dd className="tabular-nums">
            {(result.p50_rtt_us / 1000).toFixed(2)} ms
          </dd>
          <dt className="text-[var(--color-muted)]">
            {tr("speed_test_p95", undefined, "p95")}
          </dt>
          <dd className="tabular-nums">
            {(result.p95_rtt_us / 1000).toFixed(2)} ms
          </dd>
        </dl>
      )}
      <p className="mt-3 text-[10px] text-[var(--color-muted)]">
        {tr(
          "speed_test_explainer",
          undefined,
          "Latency only — doesn't measure shard throughput. A wired Ethernet LAN typically shows < 1 ms p50; Wi-Fi 5+ ms; > 20 ms suggests a problem with the network path.",
        )}
      </p>
    </section>
  );
}
