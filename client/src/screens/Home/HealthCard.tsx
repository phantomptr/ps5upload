import { useEffect } from "react";
import { Link } from "react-router";
import {
  AlertTriangle,
  ArrowRight,
  CheckCircle2,
  RefreshCw,
  Stethoscope,
  XCircle,
} from "lucide-react";

import type { HealthCheck } from "../../api/ps5";
import { Card, Spinner } from "../../components";
import {
  healthFor,
  healthProblems,
  scanHealth,
  scanHealthIfStale,
  useHealthStore,
} from "../../state/health";
import { useTr } from "../../state/lang";

/** How many problems Home lists before pointing at the full check. */
const SHOWN = 3;
/** Home re-checks when its report is older than this. */
const FRESH_MS = 10 * 60_000;

export function HealthCardView({
  problems,
  scanned,
  scanning,
  onScan,
}: {
  problems: HealthCheck[];
  /** A report is held (so "nothing to show" means "all is well"). */
  scanned: boolean;
  scanning: boolean;
  onScan: () => void;
}) {
  const tr = useTr();
  const fails = problems.some((p) => p.status === "fail");
  const ok = scanned && problems.length === 0;
  return (
    <div className="mb-4" data-testid="home-health">
      <Card>
        <div className="flex flex-wrap items-center gap-3">
          <span
            className={`grid h-8 w-8 shrink-0 place-items-center rounded-[0.6rem] bg-[var(--color-surface-3)] ${
              !scanned
                ? "text-[var(--color-muted)]"
                : ok
                  ? "text-[var(--color-good)]"
                  : fails
                    ? "text-[var(--color-bad)]"
                    : "text-[var(--color-warn)]"
            }`}
          >
            {!scanned ? (
              <Stethoscope size={15} />
            ) : ok ? (
              <CheckCircle2 size={15} />
            ) : fails ? (
              <XCircle size={15} />
            ) : (
              <AlertTriangle size={15} />
            )}
          </span>
          <div className="min-w-0 flex-1">
            <h2 className="text-sm font-semibold tracking-tight">
              {!scanned
                ? tr(
                    "home_health_checking",
                    undefined,
                    "Checking that everything works…",
                  )
                : ok
                  ? tr("home_health_ok", undefined, "Everything checks out")
                  : problems.length === 1
                    ? tr(
                        "home_health_one",
                        undefined,
                        "1 thing needs attention",
                      )
                    : tr(
                        "home_health_some",
                        { count: problems.length },
                        "{count} things need attention",
                      )}
            </h2>
            <p className="mt-0.5 text-[0.6875rem] text-[var(--color-muted)]">
              {tr(
                "home_health_desc",
                undefined,
                "Ports, connection, storage and set-up, checked for this console.",
              )}
            </p>
          </div>
          <button
            type="button"
            onClick={onScan}
            disabled={scanning}
            className="inline-flex min-h-9 items-center gap-1.5 rounded-[var(--radius-control)] px-2.5 text-xs text-[var(--color-muted)] hover:text-[var(--color-text)] disabled:opacity-60"
          >
            {scanning ? (
              <Spinner size={12} />
            ) : (
              <RefreshCw size={12} aria-hidden />
            )}
            {tr("home_health_recheck", undefined, "Check again")}
          </button>
          <Link
            to="/health"
            className="inline-flex min-h-9 shrink-0 items-center gap-1.5 rounded-[var(--radius-control)] border border-[var(--color-border)] bg-[var(--color-surface-raised)] px-3 text-xs font-semibold hover:bg-[var(--color-surface-3)]"
          >
            {tr("home_health_open", undefined, "Health check and speed test")}
            <ArrowRight size={13} aria-hidden />
          </Link>
        </div>
        {problems.length > 0 && (
          <ul className="mt-3 grid gap-2">
            {problems.slice(0, SHOWN).map((p) => (
              <li
                key={p.id}
                className={`rounded-lg border px-3 py-2 text-xs ${
                  p.status === "fail"
                    ? "border-[var(--color-bad)]/50"
                    : "border-[var(--color-warn)]/40"
                }`}
              >
                <div className="font-medium text-[var(--color-text)]">
                  {p.title}
                </div>
                <div className="mt-0.5 text-[var(--color-muted)]">
                  {p.detail}
                </div>
                {p.remedy && (
                  <div className="mt-1 text-[var(--color-text)]">
                    {p.remedy}
                  </div>
                )}
              </li>
            ))}
            {problems.length > SHOWN && (
              <li className="text-xs text-[var(--color-muted)]">
                {tr(
                  "home_health_more",
                  { count: problems.length - SHOWN },
                  "{count} more in the health check.",
                )}
              </li>
            )}
          </ul>
        )}
      </Card>
    </div>
  );
}

/** Home's health line for `host`: what needs attention, straight from the health scan the
 *  Health screen uses. Scans when it has nothing recent. */
export function HealthCard({ host }: { host: string }) {
  const health = useHealthStore((s) => healthFor(s, host));
  useEffect(() => {
    void scanHealthIfStale(host, FRESH_MS);
  }, [host]);
  return (
    <HealthCardView
      problems={healthProblems(health?.report ?? null)}
      scanned={!!health?.report}
      scanning={health?.scanning ?? false}
      onScan={() => void scanHealth(host)}
    />
  );
}
