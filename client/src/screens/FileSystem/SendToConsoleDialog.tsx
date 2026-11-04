import { useEffect, useMemo, useState } from "react";
import { Activity, ArrowUp, Folder, HardDrive, Monitor, Send, Usb } from "lucide-react";

import { fetchVolumes, fsListDir, type Volume } from "../../api/ps5";
import { Button, Modal, SegmentedControl } from "../../components";
import { consoleAddr, hostOf } from "../../lib/addr";
import { planConsoleSend, type SendItem } from "../../lib/consoleSend";
import { formatBytes } from "../../lib/format";
import { quickDestinations } from "../../lib/moveTo";
import { useTr } from "../../state/lang";
import { useRosterStore } from "../../state/roster";
import { useC2cRoutes, type C2cRoute, type C2cRouteMode } from "../../state/c2cRoutes";
import { testPs5Connection } from "../../api/ava1";
import { testLines, type TestLine } from "../../lib/c2cTest";
import { consoleParentDir as parentDir } from "../../lib/consolePath";
import { humanizeCaught } from "../../lib/humanizeError";

/** Send the selection to another console in the roster (#433): pick the console and the folder
 *  on it, see before anything runs whether it fits and what it replaces, then queue it there. */
export function SendToConsoleDialog({
  fromHost,
  items,
  startDir,
  onCancel,
  onConfirm,
}: {
  /** The console the items are on. */
  fromHost: string;
  items: SendItem[];
  /** The folder the items are in: the default destination on the other console. */
  startDir: string;
  onCancel: () => void;
  onConfirm: (targetHost: string, destDir: string, dests: string[]) => void;
}) {
  const tr = useTr();
  const profiles = useRosterStore((s) => s.profiles);
  const others = useMemo(
    () => profiles.filter((p) => hostOf(p.host) !== hostOf(fromHost)),
    [profiles, fromHost],
  );
  const [target, setTarget] = useState(() => hostOf(others[0]?.host ?? ""));
  const [volumes, setVolumes] = useState<Volume[] | null>(null);
  const [reachError, setReachError] = useState<string | null>(null);
  const [dest, setDest] = useState(startDir);
  const [listing, setListing] = useState<{ dirs: string[]; names: string[] } | null>(null);
  const [missing, setMissing] = useState(false);
  // #444: how it goes, remembered for this pair of consoles.
  const routeFor = useC2cRoutes((s) => s.routeFor);
  const setRoute = useC2cRoutes((s) => s.setRoute);
  const route = routeFor(hostOf(fromHost), target);
  const [routeOpen, setRouteOpen] = useState(() => route.mode !== "auto" || route.directAddr !== "");
  const addrOk = route.directAddr === "" || /^(\d{1,3}\.){3}\d{1,3}(:\d{1,5})?$/.test(route.directAddr);

  // The other console's drives: also how we learn it answers at all.
  useEffect(() => {
    if (!target) return;
    let live = true;
    setVolumes(null);
    setReachError(null);
    fetchVolumes(consoleAddr(target))
      .then((v) => live && setVolumes(v.filter((x) => x.writable && !x.is_placeholder)))
      .catch((e) => {
        if (!live) return;
        setVolumes([]);
        setReachError(humanizeCaught(e));
      });
    return () => {
      live = false;
    };
  }, [target]);

  // What is in the folder on the other console: its subfolders to browse, and the names a send
  // would replace. A folder that is not there yet is created by the send.
  useEffect(() => {
    if (!target || reachError) return;
    let live = true;
    setListing(null);
    setMissing(false);
    fsListDir(consoleAddr(target), dest)
      .then((entries) => {
        if (!live) return;
        setListing({
          dirs: entries
            .filter((e) => e.kind === "dir")
            .map((e) => e.name)
            .sort((a, b) => a.localeCompare(b)),
          names: entries.map((e) => e.name),
        });
      })
      .catch(() => {
        if (!live) return;
        setListing({ dirs: [], names: [] });
        setMissing(true);
      });
    return () => {
      live = false;
    };
  }, [target, dest, reachError]);

  const targetName =
    others.find((p) => hostOf(p.host) === target)?.name || target;
  const plan = planConsoleSend(items, dest, volumes ?? [], listing?.names ?? []);
  const blocked = !target || !!reachError || volumes === null || plan.fits === "no" || !addrOk;
  const what =
    items.length === 1
      ? `"${items[0].name}"`
      : tr("fs_move_n_items", { count: items.length }, `${items.length} items`);
  const quick = quickDestinations(volumes ?? []);

  return (
    <Modal
      open
      onClose={onCancel}
      size="lg"
      titleIcon={<Send size={18} />}
      title={tr("fs_send_title", { what }, `Send ${what} to another console`)}
      footer={
        <div className="flex w-full flex-wrap items-center justify-end gap-2">
          <Button variant="ghost" onClick={onCancel}>
            {tr("cancel", undefined, "Cancel")}
          </Button>
          <Button
            variant="primary"
            leftIcon={<Send size={14} />}
            disabled={blocked}
            onClick={() => onConfirm(target, dest, plan.dests)}
            data-testid="send-console-confirm"
          >
            {tr("fs_send_to", { name: targetName }, `Send to ${targetName}`)}
          </Button>
        </div>
      }
    >
      <div className="grid gap-4">
        <div>
          <div className="mb-1 text-[11px] font-semibold uppercase text-[var(--color-muted)]">
            {tr("fs_send_console", undefined, "To")}
          </div>
          <div className="grid gap-1 sm:grid-cols-2" role="radiogroup">
            {others.map((p) => {
              const h = hostOf(p.host);
              const on = h === target;
              return (
                <button
                  key={h}
                  type="button"
                  role="radio"
                  aria-checked={on}
                  onClick={() => {
                    setTarget(h);
                    setDest(startDir);
                  }}
                  className={`border rounded-[var(--radius-card)] transition-[background-color,border-color,box-shadow] flex items-center gap-2 px-3.5 py-3 text-start text-sm ${
                    on
                      ? "border-[color-mix(in_srgb,var(--color-accent)_45%,transparent)] bg-[var(--color-surface-raised)] shadow-[var(--edge-highlight),var(--shadow-1)] ring-1 ring-[color-mix(in_srgb,var(--color-accent)_30%,transparent)]"
                      : "border-[var(--color-border)] bg-[var(--color-surface)] hover:bg-[var(--color-surface-raised)]"
                  }`}
                >
                  <Monitor size={16} className="shrink-0 text-[var(--color-muted)]" />
                  <span className="min-w-0 flex-1">
                    <span className="block truncate font-medium">{p.name || h}</span>
                    <span className="block truncate font-mono text-[11px] text-[var(--color-muted)]">
                      {h}
                    </span>
                  </span>
                </button>
              );
            })}
          </div>
          {reachError && (
            <p className="mt-1.5 text-xs text-[var(--color-bad)]" role="alert">
              {tr(
                "fs_send_unreachable",
                { name: targetName },
                `${targetName} isn't answering. Check that it is on and its helper is running.`,
              )}
            </p>
          )}
        </div>

        {!reachError && (
          <>
            {quick.length > 0 && (
              <div>
                <div className="mb-1 text-[11px] font-semibold uppercase text-[var(--color-muted)]">
                  {tr("fs_send_drives", { name: targetName }, `Drives on ${targetName}`)}
                </div>
                <div className="grid gap-1 sm:grid-cols-2">
                  {quick.map(({ path, volume }) => {
                    const external =
                      volume.path.startsWith("/mnt/usb") || volume.path.startsWith("/mnt/ext");
                    const Icon = external ? Usb : HardDrive;
                    return (
                      <button
                        key={path}
                        type="button"
                        onClick={() => setDest(path)}
                        className={`border rounded-[var(--radius-card)] transition-[background-color,border-color,box-shadow] flex w-full items-center gap-2 px-3.5 py-2.5 text-start text-xs ${
                          dest === path
                            ? "border-[color-mix(in_srgb,var(--color-accent)_45%,transparent)] bg-[var(--color-surface-raised)] shadow-[var(--edge-highlight),var(--shadow-1)] ring-1 ring-[color-mix(in_srgb,var(--color-accent)_30%,transparent)]"
                            : "border-[var(--color-border)] bg-[var(--color-surface)] hover:bg-[var(--color-surface-raised)]"
                        }`}
                      >
                        <Icon size={14} className="shrink-0" />
                        <span className="min-w-0 flex-1 truncate font-mono">{path}</span>
                        <span className="shrink-0 text-[var(--color-muted)]">
                          {formatBytes(volume.free_bytes)} {tr("fs_free", undefined, "free")}
                        </span>
                      </button>
                    );
                  })}
                </div>
              </div>
            )}

            <div>
              <div className="mb-1 flex items-center gap-2">
                <button
                  type="button"
                  onClick={() => setDest(parentDir(dest))}
                  disabled={dest === "/"}
                  aria-label={tr("fs_up", undefined, "Up")}
                  className="rounded-full border shadow-[var(--edge-highlight),var(--shadow-1)] hover:bg-[var(--color-float)] border-[var(--glass-edge)] bg-[var(--color-surface-raised)] p-1 disabled:opacity-40"
                >
                  <ArrowUp size={14} />
                </button>
                <span className="min-w-0 flex-1 truncate font-mono text-xs" title={dest} data-testid="send-console-dest">
                  {dest}
                </span>
              </div>
              <div className="max-h-48 overflow-y-auto rounded-[var(--radius-card)] border border-[var(--glass-edge)]">
                {listing === null ? (
                  <div className="p-2 text-xs text-[var(--color-muted)]">
                    {tr("loading", undefined, "Loading…")}
                  </div>
                ) : listing.dirs.length === 0 ? (
                  <div className="p-2 text-xs text-[var(--color-muted)]">
                    {missing
                      ? tr("fs_send_new_folder", undefined, "This folder isn't there yet: the send creates it.")
                      : tr("fs_move_no_subfolders", undefined, "No folders inside.")}
                  </div>
                ) : (
                  listing.dirs.map((name) => (
                    <button
                      key={name}
                      type="button"
                      onClick={() => setDest(dest === "/" ? `/${name}` : `${dest}/${name}`)}
                      className="flex w-full items-center gap-2 px-2 py-1.5 text-start text-xs hover:bg-[var(--color-surface-3)]"
                    >
                      <Folder size={14} className="shrink-0 text-[var(--color-muted)]" />
                      <span className="truncate">{name}</span>
                    </button>
                  ))
                )}
              </div>
            </div>
          </>
        )}

        <div
          className={`grid gap-1 rounded-[var(--radius-field)] border px-3 py-2 text-sm ${
            plan.fits === "no"
              ? "border-[color-mix(in_srgb,var(--color-bad)_40%,transparent)] bg-[var(--color-bad-soft)]"
              : plan.fits === "tight" || plan.replacing.length > 0
                ? "border-[color-mix(in_srgb,var(--color-warn)_40%,transparent)] bg-[var(--color-warn-soft)]"
                : "border-[var(--color-border)] bg-[var(--color-surface-2)]"
          }`}
          data-testid="send-console-summary"
        >
          <span>
            {plan.fits === "no"
              ? tr(
                  "fs_send_no_room",
                  { size: formatBytes(plan.bytes), name: targetName },
                  `${formatBytes(plan.bytes)} won't fit there on ${targetName}.`,
                )
              : plan.bytes > 0
                ? tr(
                    "fs_send_summary",
                    { size: formatBytes(plan.bytes), name: targetName },
                    `Copies ${formatBytes(plan.bytes)} to ${targetName}. The originals stay here.`,
                  )
                : tr(
                    "fs_send_summary_folders",
                    { name: targetName },
                    `Copies to ${targetName}. The originals stay here.`,
                  )}
            {plan.fits === "tight" &&
              ` ${tr("fs_move_tight", undefined, "It may not fit: the PS5 holds back more space as it writes.")}`}
          </span>
          {plan.replacing.length > 0 && (
            <span>
              {tr(
                "fs_send_replacing",
                { names: plan.replacing.join(", ") },
                `Already there and replaced: ${plan.replacing.join(", ")}.`,
              )}
            </span>
          )}
          <span className="text-xs text-[var(--color-muted)]">
            {route.mode === "direct"
              ? tr("fs_send_route_direct", undefined, "Goes straight between the consoles, never through this device; if they cannot connect, the send fails and says why.")
              : route.mode === "relay"
                ? tr("fs_send_route_relay", undefined, "Goes through this device.")
                : tr(
                    "fs_send_route",
                    undefined,
                    "Goes straight between the consoles when they can reach each other, otherwise through this computer.",
                  )}{" "}
            <button
              type="button"
              className="text-[var(--color-accent)] underline-offset-2 hover:underline"
              aria-expanded={routeOpen}
              onClick={() => setRouteOpen((v) => !v)}
            >
              {tr("fs_send_route_change", undefined, "Change…")}
            </button>
          </span>
        </div>

        {routeOpen && target && (
          <div className="grid gap-2 text-xs" data-testid="send-console-route">
            <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
              <span className="min-w-28 text-[var(--color-muted)]">{tr("fs_send_route_label", undefined, "Route")}</span>
              <SegmentedControl
                ariaLabel={tr("fs_send_route_label", undefined, "Route")}
                value={route.mode}
                onChange={(v) => setRoute(hostOf(fromHost), target, { ...route, mode: v as C2cRouteMode })}
                segments={[
                  { value: "auto", label: tr("fs_send_route_auto", undefined, "Auto") },
                  { value: "direct", label: tr("fs_send_route_direct_only", undefined, "Direct only") },
                  { value: "relay", label: tr("fs_send_route_via_here", undefined, "Through this device") },
                ]}
              />
            </div>
            {route.mode !== "relay" && (
              <label className="grid gap-1">
                <span className="text-[var(--color-muted)]">
                  {tr("fs_send_dial_label", { name: targetName }, `Address the sending console dials to reach ${targetName} (optional)`)}
                </span>
                <input
                  type="text"
                  inputMode="url"
                  autoCapitalize="off"
                  autoCorrect="off"
                  spellCheck={false}
                  value={route.directAddr}
                  placeholder={`${target}:9120`}
                  onChange={(e) => setRoute(hostOf(fromHost), target, { ...route, directAddr: e.target.value })}
                  className={`input font-mono ${addrOk ? "" : "border-[var(--color-bad)]"}`}
                  aria-invalid={!addrOk}
                  data-testid="send-console-dial"
                />
                <span className="text-[var(--color-muted)]">
                  {tr(
                    "fs_send_dial_hint",
                    undefined,
                    "Leave it empty to dial the console's own address. For ps5-tailscale, set a local forward on the sending console (its status page: 127.0.0.1:19120 → the other console:9120) and enter 127.0.0.1:19120 here. Who may send is decided by the consoles' keys and a one-time ticket, not by the address.",
                  )}
                </span>
                {!addrOk && (
                  <span className="text-[var(--color-bad)]" role="alert">
                    {tr("fs_send_dial_bad", undefined, "An IPv4 address, with an optional :port.")}
                  </span>
                )}
              </label>
            )}
          </div>
        )}

        {target && (
          <ConnectionTestBlock
            from={hostOf(fromHost)}
            to={target}
            fromName={profiles.find((p) => hostOf(p.host) === hostOf(fromHost))?.name || hostOf(fromHost)}
            toName={targetName}
            route={route}
            disabled={!addrOk}
          />
        )}
      </div>
    </Modal>
  );
}

/** "Test connection" (#444): asks the sending console to connect to the receiving one the way
 *  a send would dial it (and, unless direct only, whether this device reaches both). */
function ConnectionTestBlock({
  from,
  to,
  fromName,
  toName,
  route,
  disabled,
}: {
  from: string;
  to: string;
  fromName: string;
  toName: string;
  route: C2cRoute;
  disabled: boolean;
}) {
  const tr = useTr();
  const [deep, setDeep] = useState(false);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<{ lines: TestLine[]; error: string | null } | null>(null);
  // Another console or route makes an earlier answer stale.
  useEffect(() => setResult(null), [to, route.mode, route.directAddr]);
  const run = async () => {
    setBusy(true);
    setResult(null);
    try {
      const t = await testPs5Connection(from, to, route, deep && route.mode !== "relay");
      setResult({ lines: testLines(t, fromName, toName), error: null });
    } catch (e) {
      setResult({ lines: [], error: humanizeCaught(e) });
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="grid gap-1.5 text-xs" data-testid="send-console-test">
      <div className="flex flex-wrap items-center gap-3">
        <Button
          variant="secondary"
          size="sm"
          leftIcon={<Activity size={14} />}
          loading={busy}
          disabled={disabled}
          onClick={() => void run()}
          data-testid="send-console-test-run"
        >
          {busy
            ? tr("c2c_test_running", undefined, "Testing…")
            : tr("c2c_test_button", undefined, "Test connection")}
        </Button>
        {route.mode !== "relay" && (
          <label className="flex items-center gap-1.5 text-[var(--color-muted)]">
            <input type="checkbox" checked={deep} onChange={(e) => setDeep(e.target.checked)} />
            {tr("c2c_test_deep", undefined, "Also send 4 KiB straight across")}
          </label>
        )}
      </div>
      {result && <TestResultLines lines={result.lines} error={result.error} />}
    </div>
  );
}

/** The test's verdicts, one line each. */
export function TestResultLines({ lines, error }: { lines: TestLine[]; error: string | null }) {
  const tr = useTr();
  if (error) {
    return (
      <p className="text-[var(--color-bad)]" role="alert">
        {tr("c2c_test_failed", { error }, `The test did not run: ${error}`)}
      </p>
    );
  }
  return (
    <ul className="grid gap-0.5" data-testid="send-console-test-result">
      {lines.map((l) => (
        <li
          key={l.key + JSON.stringify(l.vars)}
          className={
            l.tone === "good"
              ? "text-[var(--color-good)]"
              : l.tone === "bad"
                ? "text-[var(--color-bad)]"
                : "text-[var(--color-warn)]"
          }
        >
          {tr(l.key, l.vars, l.fallback)}
        </li>
      ))}
    </ul>
  );
}
