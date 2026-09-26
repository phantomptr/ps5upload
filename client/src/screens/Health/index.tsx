import { useCallback, useEffect, useMemo, useState } from "react";
import {
  Stethoscope,
  RefreshCw,
  CheckCircle2,
  AlertTriangle,
  XCircle,
  MinusCircle,
  Wrench,
  Trash2,
} from "lucide-react";
import {
  PageHeader,
  Button,
  Card,
  ErrorCard,
  ConnectionGate,
  Modal,
  Spinner,
} from "../../components";
import { useTr } from "../../state/lang";
import { mgmtAddr } from "../../lib/addr";
import { useStaleHostGuard } from "../../lib/staleHostGuard";
import {
  healthScan,
  healthJunk,
  healthFix,
  type HealthReport,
  type HealthCheck,
  type HealthStatus,
  type HealthFixAction,
  type HealthJunkFile,
} from "../../api/ps5";

/** Visual treatment per status.
 *
 *  `skip` is deliberately muted rather than coloured. It means "we
 *  could not measure this here", which on retail firmware is common and
 *  entirely normal — painting it as a problem is how a health screen
 *  teaches people to ignore it. */
const STATUS_UI: Record<
  HealthStatus,
  { icon: typeof CheckCircle2; cls: string; ring: string }
> = {
  pass: {
    icon: CheckCircle2,
    cls: "text-[var(--color-good)]",
    ring: "border-[var(--color-border)]",
  },
  warn: {
    icon: AlertTriangle,
    cls: "text-[var(--color-warn)]",
    ring: "border-[var(--color-warn)]/40",
  },
  fail: {
    icon: XCircle,
    cls: "text-[var(--color-bad)]",
    ring: "border-[var(--color-bad)]/50",
  },
  skip: {
    icon: MinusCircle,
    cls: "text-[var(--color-muted)]",
    ring: "border-[var(--color-border)]",
  },
};

const CATEGORY_ORDER = [
  "connectivity",
  "runtime",
  "storage",
  "system",
  "remoteplay",
  "hygiene",
] as const;

function humanBytes(n: number): string {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return i === 0 ? `${n} B` : `${v.toFixed(1)} ${units[i]}`;
}

export default function HealthScreen() {
  const tr = useTr();
  // The address is taken from the guard's captured host inside each
  // async call (see below), not read here, so a console switch mid-scan
  // cannot retarget or misattribute the result.
  // Results must never land on a console the user has since
  // switched away from -- a health verdict shown against the
  // wrong console is worse than none.
  const guard = useStaleHostGuard();

  const [report, setReport] = useState<HealthReport | null>(null);
  const [scanning, setScanning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [fixing, setFixing] = useState<HealthFixAction | null>(null);
  const [note, setNote] = useState<string | null>(null);

  // Junk confirmation: the file list is fetched and shown before
  // anything is deleted. A cleanup button that does not say what it
  // will destroy is not one anybody should press.
  const [junkOpen, setJunkOpen] = useState(false);
  const [junk, setJunk] = useState<HealthJunkFile[] | null>(null);
  const [junkTotal, setJunkTotal] = useState(0);

  const scan = useCallback(async () => {
    setScanning(true);
    setError(null);
    setNote(null);
    const probe = guard.capture();
    try {
      const r = await healthScan(probe.host ? mgmtAddr(probe.host) : undefined);
      if (probe.isStale()) return;
      setReport(r);
    } catch (e) {
      if (probe.isStale()) return;
      setError(String(e));
    } finally {
      setScanning(false);
    }
  }, [guard]);

  useEffect(() => {
    void scan();
  }, [scan]);

  const runFix = useCallback(
    async (action: HealthFixAction) => {
      setFixing(action);
      setError(null);
      setNote(null);
      const probe = guard.capture();
      try {
        // A failed repair throws, carrying any partial progress in the
        // message, so the catch below reports it rather than the screen
        // quietly showing success.
        const out = await healthFix(
          action,
          probe.host ? mgmtAddr(probe.host) : undefined,
        );
        if (probe.isStale()) return;
        setNote(
          out.changed.length
            ? out.changed.join("; ")
            : tr("health_fix_done", undefined, "Done."),
        );
        await scan();
      } catch (e) {
        if (probe.isStale()) return;
        setError(String(e));
      } finally {
        setFixing(null);
      }
    },
    [guard, scan, tr],
  );

  const openJunk = useCallback(async () => {
    setError(null);
    const probe = guard.capture();
    try {
      const r = await healthJunk(probe.host ? mgmtAddr(probe.host) : undefined);
      if (probe.isStale()) return;
      setJunk(r.files);
      setJunkTotal(r.total_bytes);
      setJunkOpen(true);
    } catch (e) {
      if (probe.isStale()) return;
      setError(String(e));
    }
  }, [guard]);

  const onFix = useCallback(
    (action: HealthFixAction) => {
      if (action === "clean_junk") {
        void openJunk();
        return;
      }
      void runFix(action);
    },
    [openJunk, runFix],
  );

  const grouped = useMemo(() => {
    if (!report) return [];
    return CATEGORY_ORDER.map((cat) => ({
      cat,
      checks: report.checks.filter((c) => c.category === cat),
    })).filter((g) => g.checks.length > 0);
  }, [report]);

  const s = report?.summary;
  const allClear = s ? s.fail === 0 && s.warn === 0 : false;

  return (
    <div className="mx-auto max-w-4xl p-6">
      <PageHeader
        icon={Stethoscope}
        title={tr("health_title", undefined, "Health Check")}
        description={tr(
          "health_subtitle",
          undefined,
          "Check that your console and this app are set up and working correctly.",
        )}
        right={
          <Button
            variant="secondary"
            size="sm"
            leftIcon={<RefreshCw size={14} />}
            onClick={() => void scan()}
            disabled={scanning}
          >
            {tr("health_rescan", undefined, "Scan again")}
          </Button>
        }
      />

      <ConnectionGate>
        {error && <ErrorCard title={error} />}

        {note && (
          <Card className="mb-4 border-[var(--color-good)]/40 p-3 text-sm">
            {note}
          </Card>
        )}

        {scanning && !report && (
          <div className="flex items-center gap-3 p-6 text-sm text-[var(--color-muted)]">
            <Spinner />
            {tr("health_scanning", undefined, "Checking your console…")}
          </div>
        )}

        {s && (
          <Card className="mb-5 p-4">
            <div className="flex flex-wrap items-center gap-4">
              <div
                className={
                  allClear
                    ? "text-[var(--color-good)]"
                    : s.fail > 0
                      ? "text-[var(--color-bad)]"
                      : "text-[var(--color-warn)]"
                }
              >
                {allClear ? (
                  <CheckCircle2 size={28} />
                ) : s.fail > 0 ? (
                  <XCircle size={28} />
                ) : (
                  <AlertTriangle size={28} />
                )}
              </div>
              <div className="min-w-0 flex-1">
                <div className="text-base font-semibold">
                  {allClear
                    ? tr(
                        "health_all_good",
                        undefined,
                        "Everything looks right",
                      )
                    : s.fail > 0
                      ? tr(
                          "health_has_failures",
                          undefined,
                          "Something needs attention",
                        )
                      : tr(
                          "health_has_warnings",
                          undefined,
                          "A few things worth a look",
                        )}
                </div>
                <div className="mt-0.5 text-xs text-[var(--color-muted)]">
                  {`${s.pass} passed · ${s.warn} warnings · ${s.fail} problems · ${s.skip} not applicable`}
                </div>
              </div>
            </div>
          </Card>
        )}

        {grouped.map((g) => (
          <section key={g.cat} className="mb-5">
            <h2 className="mb-2 text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
              {tr(`health_cat_${g.cat}`, undefined, g.cat)}
            </h2>
            <div className="flex flex-col gap-2">
              {g.checks.map((c) => (
                <CheckRow
                  key={c.id}
                  check={c}
                  busy={fixing === c.fix}
                  onFix={onFix}
                  tr={tr}
                />
              ))}
            </div>
          </section>
        ))}

        <Modal
          open={junkOpen}
          onClose={() => setJunkOpen(false)}
          title={tr("health_junk_title", undefined, "Delete leftover files?")}
        >
          <p className="text-sm text-[var(--color-muted)]">
            {tr(
              "health_junk_explain",
              undefined,
              "These are unfinished files left behind by interrupted transfers. Only files ps5upload created are listed, and nothing outside its own folders is touched.",
            )}
          </p>
          <div className="mt-3 max-h-64 overflow-auto rounded border border-[var(--color-border)]">
            {junk && junk.length > 0 ? (
              junk.map((f) => (
                <div
                  key={f.path}
                  className="flex items-center justify-between gap-3 border-b border-[var(--color-border)] px-3 py-2 text-xs last:border-b-0"
                >
                  <code className="min-w-0 truncate font-mono">{f.path}</code>
                  <span className="shrink-0 text-[var(--color-muted)]">
                    {humanBytes(f.size)}
                  </span>
                </div>
              ))
            ) : (
              <div className="px-3 py-4 text-xs text-[var(--color-muted)]">
                {tr("health_junk_none", undefined, "Nothing to clean.")}
              </div>
            )}
          </div>
          <div className="mt-4 flex items-center justify-between gap-3">
            <span className="text-xs text-[var(--color-muted)]">
              {junk?.length
                ? `${junk.length} file(s), ${humanBytes(junkTotal)}`
                : ""}
            </span>
            <div className="flex gap-2">
              <Button
                variant="secondary"
                size="sm"
                onClick={() => setJunkOpen(false)}
              >
                {tr("cancel", undefined, "Cancel")}
              </Button>
              <Button
                variant="danger"
                size="sm"
                leftIcon={<Trash2 size={14} />}
                disabled={!junk?.length || fixing === "clean_junk"}
                onClick={() => {
                  setJunkOpen(false);
                  void runFix("clean_junk");
                }}
              >
                {tr("health_junk_delete", undefined, "Delete them")}
              </Button>
            </div>
          </div>
        </Modal>
      </ConnectionGate>
    </div>
  );
}

function CheckRow({
  check,
  busy,
  onFix,
  tr,
}: {
  check: HealthCheck;
  busy: boolean;
  onFix: (a: HealthFixAction) => void;
  tr: ReturnType<typeof useTr>;
}) {
  const ui = STATUS_UI[check.status];
  const Icon = ui.icon;
  return (
    <div
      className={`flex items-start gap-3 rounded-lg border ${ui.ring} bg-[var(--color-surface-2)] p-3`}
    >
      <Icon size={16} className={`mt-0.5 shrink-0 ${ui.cls}`} />
      <div className="min-w-0 flex-1">
        <div className="text-sm font-medium">{check.title}</div>
        <div className="mt-0.5 text-xs text-[var(--color-muted)]">
          {check.detail}
        </div>
        {check.remedy && (
          <div className="mt-1.5 text-xs text-[var(--color-text)]">
            {check.remedy}
          </div>
        )}
      </div>
      {check.fix && (
        <Button
          variant="secondary"
          size="sm"
          className="shrink-0"
          leftIcon={<Wrench size={13} />}
          disabled={busy}
          onClick={() => onFix(check.fix as HealthFixAction)}
        >
          {busy
            ? tr("health_fixing", undefined, "Fixing…")
            : tr("health_fix", undefined, "Fix")}
        </Button>
      )}
    </div>
  );
}
