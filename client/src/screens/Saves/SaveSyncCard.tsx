// Save sync (engine save_sync.rs): one user's saves kept the same on two PS5s. Set up a pair,
// Check now, and send the saves that changed; nothing is copied without the user saying so, and
// the save that gets replaced is backed up first.
import { useCallback, useEffect, useState } from "react";
import { ArrowLeftRight, RefreshCw, Trash2 } from "lucide-react";

import { Badge, Button, Checkbox, Select } from "../../components";
import { profileInfo, type ProfileUser } from "../../api/ps5";
import {
  saveSyncApi,
  type SyncDirection,
  type SyncPair,
  type SyncRow,
  type SyncStatus,
} from "../../api/saveSync";
import { consoleAddr, hostOf } from "../../lib/addr";
import { formatAgo } from "../../lib/formatDate";
import { formatBytes } from "../../lib/format";
import { humanizeCaught } from "../../lib/humanizeError";
import { useRosterStore } from "../../state/roster";
import { useTr } from "../../state/lang";
import { useSaveGameName } from "./gameNames";
import { userHex } from "./saveUsers";
import {
  applyItems,
  canSync,
  consoleName,
  defaultDirection,
  directionsFor,
  initialSelection,
  needsFirstStart,
  needsPick,
  sortRows,
  statusTone,
} from "./saveSync";

type Tr = ReturnType<typeof useTr>;

function statusLabel(tr: Tr, s: SyncStatus, a: string, b: string): string {
  switch (s) {
    case "in_step":
      return tr("saves_sync_in_step", undefined, "In step");
    case "a_newer":
      return tr("saves_sync_newer", { console: a }, "Newer on {console}");
    case "b_newer":
      return tr("saves_sync_newer", { console: b }, "Newer on {console}");
    case "both_changed":
      return tr("saves_sync_both_changed", undefined, "Changed on both");
    case "first_time":
      return tr("saves_sync_first_time", undefined, "Not synced yet");
    case "only_on_a":
      return tr("saves_sync_only_on", { console: a }, "Only on {console}");
    case "only_on_b":
      return tr("saves_sync_only_on", { console: b }, "Only on {console}");
    case "excluded":
      return tr("saves_sync_excluded", undefined, "Left out");
    default:
      return tr("saves_sync_unavailable", undefined, "Not now");
  }
}

function reasonText(tr: Tr, row: SyncRow, a: string, b: string): string | null {
  const where = row.reason_side === "b" ? b : a;
  switch (row.reason) {
    case "offline":
      return tr("saves_sync_reason_offline", { console: where }, "{console} is off, resting or not answering.");
    case "settling":
      return tr("saves_sync_reason_settling", { console: where }, "{console} has just woken up. Try again in a minute.");
    case "running":
      return tr("saves_sync_reason_running", { console: where }, "The game is running on {console}. Close it first.");
    case "ps4":
      return tr("saves_sync_reason_ps4", undefined, "PS4 saves can't be copied to another console.");
    case "error":
      return row.last_error;
    default:
      return null;
  }
}

function SideLine({ tr, name, side }: { tr: Tr; name: string; side: SyncRow["a"] }) {
  if (!side) {
    return (
      <span className="text-[var(--color-muted)]">
        {tr("saves_sync_side_none", { console: name }, "{console}: no save")}
      </span>
    );
  }
  const changed = side.changed_ms
    ? tr("saves_sync_side_changed", { ago: formatAgo(side.changed_ms) }, "changed about {ago}")
    : null;
  return (
    <span>
      {name}: {formatBytes(side.size)}
      {changed ? <span className="text-[var(--color-muted)]"> · {changed}</span> : null}
    </span>
  );
}

function SyncRowView({
  row,
  a,
  b,
  selected,
  pick,
  result,
  busy,
  onSelect,
  onPick,
  onExclude,
}: {
  row: SyncRow;
  a: string;
  b: string;
  selected: boolean;
  pick: SyncDirection | undefined;
  result: { ok: boolean; error?: string } | undefined;
  busy: boolean;
  onSelect: (on: boolean) => void;
  onPick: (d: SyncDirection | undefined) => void;
  onExclude: (out: boolean) => void;
}) {
  const tr = useTr();
  const game = useSaveGameName(row.title_id).name ?? row.title_id;
  const sendTo = (d: SyncDirection) =>
    d === "a_to_b"
      ? tr("saves_sync_send_to", { console: b }, "Send to {console}")
      : tr("saves_sync_send_to", { console: a }, "Send to {console}");
  const first = needsFirstStart(row);
  const reason = reasonText(tr, row, a, b);
  const direction = pick ?? defaultDirection(row);
  return (
    <li className="flex flex-col gap-1 rounded-[var(--radius-field)] border border-[var(--color-border)] bg-[var(--color-surface)] p-2 text-sm">
      <div className="flex flex-wrap items-center gap-2">
        <span className="min-w-0 flex-1 basis-[10rem] truncate font-medium" title={row.title_id}>
          {game}
        </span>
        <Badge tone={statusTone(row.status)} size="sm">
          {statusLabel(tr, row.status, a, b)}
        </Badge>
      </div>
      {row.status !== "excluded" && (
        <div className="flex flex-col gap-0.5 text-xs sm:flex-row sm:flex-wrap sm:gap-x-4">
          <SideLine tr={tr} name={a} side={row.a} />
          <SideLine tr={tr} name={b} side={row.b} />
        </div>
      )}
      {reason && <p className="text-xs text-[var(--color-muted)]">{reason}</p>}
      {first && (
        <p className="text-xs text-[var(--color-muted)]">
          {tr(
            "saves_sync_start_first",
            { console: first === "a" ? a : b },
            "Start the game once on {console} and save, then check again.",
          )}
        </p>
      )}
      {result && !result.ok && <p className="text-xs text-[var(--color-bad)]">{result.error}</p>}
      {result?.ok && (
        <p className="text-xs text-[var(--color-good)]">{tr("saves_sync_sent", undefined, "Sent.")}</p>
      )}
      <div className="flex flex-wrap items-center gap-2">
        {canSync(row) && (
          <Checkbox
            checked={selected && direction !== null}
            disabled={busy || direction === null}
            onChange={onSelect}
            label={direction ? sendTo(direction) : tr("saves_sync_pick_first", undefined, "Pick which to keep")}
          />
        )}
        {needsPick(row) && (
          <div className="min-w-0 flex-1 basis-[12rem]">
            <Select
              aria-label={tr("saves_sync_pick_label", undefined, "Which save to keep")}
              value={pick ?? ""}
              disabled={busy}
              onChange={(e) => onPick((e.target.value || undefined) as SyncDirection | undefined)}
              options={[
                { value: "", label: tr("saves_sync_pick_none", undefined, "Pick which to keep…") },
                ...directionsFor(row).map((d) => ({
                  value: d,
                  label:
                    d === "a_to_b"
                      ? tr("saves_sync_keep", { console: a }, "Keep {console}'s save")
                      : tr("saves_sync_keep", { console: b }, "Keep {console}'s save"),
                })),
              ]}
            />
          </div>
        )}
        <span className="flex-1" />
        <Button variant="ghost" size="sm" disabled={busy} onClick={() => onExclude(row.status !== "excluded")}>
          {row.status === "excluded"
            ? tr("saves_sync_include", undefined, "Include")
            : tr("saves_sync_leave_out", undefined, "Leave out")}
        </Button>
      </div>
    </li>
  );
}

function PairView({ pair, onRemoved }: { pair: SyncPair; onRemoved: () => void }) {
  const tr = useTr();
  const profiles = useRosterStore((s) => s.profiles);
  const [current, setCurrent] = useState(pair);
  const [rows, setRows] = useState<SyncRow[] | null>(null);
  const [checking, setChecking] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<Set<string>>(() => new Set());
  const [picks, setPicks] = useState<Record<string, SyncDirection | undefined>>({});
  const [results, setResults] = useState<Record<string, { ok: boolean; error?: string }>>({});
  const a = consoleName(current.a.host, profiles);
  const b = consoleName(current.b.host, profiles);

  const check = useCallback(async () => {
    setChecking(true);
    setError(null);
    try {
      const r = await saveSyncApi.status(current.id);
      setCurrent(r.pair);
      setRows(sortRows(r.rows));
      setSelected(initialSelection(r.rows));
      setPicks({});
    } catch (e) {
      setError(humanizeCaught(e));
    } finally {
      setChecking(false);
    }
  }, [current.id]);

  const sync = async () => {
    if (!rows) return;
    const items = applyItems(rows, selected, picks);
    setBusy(true);
    setError(null);
    const done: Record<string, { ok: boolean; error?: string }> = {};
    // One at a time, so each finished one shows at once.
    for (const item of items) {
      try {
        const r = await saveSyncApi.apply(current.id, [item]);
        done[item.key] = r.results[0] ?? { ok: false };
      } catch (e) {
        done[item.key] = { ok: false, error: humanizeCaught(e) };
      }
      setResults({ ...done });
    }
    setBusy(false);
    await check();
    setResults(done);
  };

  const exclude = async (key: string, out: boolean) => {
    const next = out ? [...new Set([...current.excluded, key])] : current.excluded.filter((k) => k !== key);
    try {
      const r = await saveSyncApi.setPair(current.a, current.b, next);
      setCurrent(r.pair);
      await check();
    } catch (e) {
      setError(humanizeCaught(e));
    }
  };

  const remove = async () => {
    try {
      await saveSyncApi.removePair(current.id);
      onRemoved();
    } catch (e) {
      setError(humanizeCaught(e));
    }
  };

  const count = rows ? applyItems(rows, selected, picks).length : 0;
  return (
    <div className="flex flex-col gap-2 rounded-[var(--radius-field)] border border-[var(--color-border)] p-3" data-testid="save-sync-pair">
      <div className="flex flex-wrap items-center gap-2">
        <ArrowLeftRight size={14} aria-hidden className="shrink-0" />
        <span className="min-w-0 flex-1 basis-[10rem] text-sm font-medium">
          {tr("saves_sync_pair_name", { a, b }, "{a} and {b}")}
        </span>
        <Button
          variant="secondary"
          size="sm"
          leftIcon={<RefreshCw size={12} />}
          loading={checking}
          disabled={busy}
          onClick={() => void check()}
        >
          {tr("saves_sync_check", undefined, "Check now")}
        </Button>
        <Button
          variant="ghost"
          size="sm"
          leftIcon={<Trash2 size={12} />}
          disabled={busy}
          onClick={() => void remove()}
        >
          {tr("saves_sync_stop", undefined, "Stop")}
        </Button>
      </div>
      {error && <p className="text-xs text-[var(--color-bad)]">{error}</p>}
      {rows && rows.length === 0 && (
        <p className="text-xs text-[var(--color-muted)]">
          {tr("saves_sync_none", undefined, "No saves on either console for these users.")}
        </p>
      )}
      {rows && rows.length > 0 && (
        <>
          <p className="text-xs text-[var(--color-muted)]">
            {tr(
              "saves_sync_times_note",
              undefined,
              "Times are when this app first saw the change, so they are approximate.",
            )}
          </p>
          <ul className="flex flex-col gap-1.5">
            {rows.map((r) => (
              <SyncRowView
                key={r.key}
                row={r}
                a={a}
                b={b}
                selected={selected.has(r.key)}
                pick={picks[r.key]}
                result={results[r.key]}
                busy={busy}
                onSelect={(on) =>
                  setSelected((s) => {
                    const n = new Set(s);
                    if (on) n.add(r.key);
                    else n.delete(r.key);
                    return n;
                  })
                }
                onPick={(d) => {
                  setPicks((p) => ({ ...p, [r.key]: d }));
                  setSelected((s) => {
                    const n = new Set(s);
                    if (d) n.add(r.key);
                    else n.delete(r.key);
                    return n;
                  });
                }}
                onExclude={(out) => void exclude(r.key, out)}
              />
            ))}
          </ul>
          <div className="flex justify-end">
            <Button variant="primary" size="sm" loading={busy} disabled={count === 0} onClick={() => void sync()}>
              {tr("saves_sync_selected", { count }, "Sync selected ({count})")}
            </Button>
          </div>
        </>
      )}
    </div>
  );
}

function userLabel(u: ProfileUser): string {
  return u.username.trim() || userHex(u.uid);
}

/** "Keep in step with another PS5": this console's user, the other console and its user. */
function PairSetup({ host, onAdded }: { host: string; onAdded: () => void }) {
  const tr = useTr();
  const profiles = useRosterStore((s) => s.profiles);
  const others = profiles.filter((p) => hostOf(p.host) !== hostOf(host));
  const [hereUsers, setHereUsers] = useState<ProfileUser[]>([]);
  const [here, setHere] = useState("");
  const [other, setOther] = useState(others[0] ? hostOf(others[0].host) : "");
  const [thereUsers, setThereUsers] = useState<ProfileUser[] | null>(null);
  const [there, setThere] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [otherDown, setOtherDown] = useState(false);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    let live = true;
    profileInfo(consoleAddr(host))
      .then((p) => {
        if (!live) return;
        setHereUsers(p.users);
        // The user at the console, else the first.
        const atConsole = p.users.find((u) => u.uid >>> 0 === p.uid >>> 0) ?? p.users[0];
        setHere(atConsole ? String(atConsole.uid >>> 0) : "");
      })
      .catch((e) => live && setError(humanizeCaught(e)));
    return () => {
      live = false;
    };
  }, [host]);

  useEffect(() => {
    if (!other) return;
    let live = true;
    setThereUsers(null);
    setOtherDown(false);
    profileInfo(consoleAddr(other))
      .then((p) => {
        if (!live) return;
        setThereUsers(p.users);
        setThere(String((p.users[0]?.uid ?? p.uid) >>> 0));
      })
      .catch(() => {
        if (!live) return;
        setThereUsers([]);
        setOtherDown(true);
      });
    return () => {
      live = false;
    };
  }, [other]);

  if (others.length === 0) {
    return (
      <p className="text-xs text-[var(--color-muted)]">
        {tr("saves_sync_no_other", undefined, "Add your other PS5 under Consoles first.")}
      </p>
    );
  }
  const add = async () => {
    setSaving(true);
    setError(null);
    try {
      await saveSyncApi.setPair(
        { host: hostOf(host), user_id: Number(here) },
        { host: other, user_id: Number(there) },
      );
      onAdded();
    } catch (e) {
      setError(humanizeCaught(e));
    } finally {
      setSaving(false);
    }
  };
  const opts = (users: readonly ProfileUser[]) =>
    users.map((u) => ({ value: String(u.uid >>> 0), label: userLabel(u) }));
  return (
    <div className="flex flex-col gap-2" data-testid="save-sync-setup">
      <p className="text-sm font-medium">{tr("saves_sync_setup", undefined, "Keep in step with another PS5")}</p>
      <div className="grid gap-2 sm:grid-cols-3">
        <Select
          label={tr("saves_sync_user_here", undefined, "User on this PS5")}
          value={here}
          onChange={(e) => setHere(e.target.value)}
          options={opts(hereUsers)}
        />
        <Select
          label={tr("saves_sync_other_console", undefined, "Other PS5")}
          value={other}
          onChange={(e) => setOther(e.target.value)}
          options={others.map((p) => ({ value: hostOf(p.host), label: p.name || hostOf(p.host) }))}
        />
        <Select
          label={tr("saves_sync_user_there", undefined, "User there")}
          value={there}
          disabled={!thereUsers || thereUsers.length === 0}
          onChange={(e) => setThere(e.target.value)}
          options={opts(thereUsers ?? [])}
        />
      </div>
      <p className="text-xs text-[var(--color-muted)]">
        {tr(
          "saves_sync_setup_note",
          undefined,
          "Every PS5 and emulator save both users have is included; you can leave some out after a check. PS4 saves can't be copied to another console.",
        )}
      </p>
      {otherDown && (
        <p className="text-xs text-[var(--color-bad)]">
          {tr(
            "saves_sync_other_offline",
            { console: consoleName(other, profiles) },
            "{console} isn't answering. Turn it on and try again.",
          )}
        </p>
      )}
      {error && <p className="text-xs text-[var(--color-bad)]">{error}</p>}
      <div className="flex justify-end">
        <Button
          variant="secondary"
          size="sm"
          loading={saving}
          disabled={!here || !there || !other}
          onClick={() => void add()}
        >
          {tr("saves_sync_add", undefined, "Keep in step")}
        </Button>
      </div>
    </div>
  );
}

/** The Saves screen's "Save sync" section, for the console shown. */
export default function SaveSyncCard({ host }: { host: string }) {
  const tr = useTr();
  const [pairs, setPairs] = useState<SyncPair[] | null>(null);
  const [setup, setSetup] = useState(false);
  const load = useCallback(async () => {
    try {
      setPairs((await saveSyncApi.view()).pairs);
    } catch {
      // An engine before save sync: no section.
      setPairs(null);
    }
  }, []);
  useEffect(() => {
    void load();
  }, [load]);
  if (!pairs) return null;
  const mine = pairs.filter((p) => hostOf(p.a.host) === hostOf(host) || hostOf(p.b.host) === hostOf(host));
  return (
    <section
      className="mb-4 rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4"
      data-testid="save-sync"
    >
      <header className="mb-1 flex flex-wrap items-center gap-2">
        <ArrowLeftRight size={14} />
        <h3 className="flex-1 basis-[8rem] text-sm font-semibold">{tr("saves_sync_title", undefined, "Save sync")}</h3>
        {mine.length > 0 && !setup && (
          <Button variant="ghost" size="sm" onClick={() => setSetup(true)}>
            {tr("saves_sync_add_another", undefined, "Add another")}
          </Button>
        )}
      </header>
      <p className="mb-3 text-xs text-[var(--color-muted)]">
        {tr(
          "saves_sync_note",
          undefined,
          "Keep a user's saves the same on two PS5s. Nothing is copied until you say so, and the save that gets replaced is backed up first. After you close a game, the app asks whether to send its save to the other PS5.",
        )}
      </p>
      <div className="flex flex-col gap-3">
        {mine.map((p) => (
          <PairView key={p.id} pair={p} onRemoved={() => void load()} />
        ))}
        {(mine.length === 0 || setup) && (
          <PairSetup
            host={host}
            onAdded={() => {
              setSetup(false);
              void load();
            }}
          />
        )}
      </div>
    </section>
  );
}
