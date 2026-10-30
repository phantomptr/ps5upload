import { useEffect, useMemo, useState } from "react";
import { ArrowRightLeft, CheckCircle2, CircleAlert, ShieldCheck } from "lucide-react";

import { Button, Card, Checkbox, Input, Select, Spinner } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import { profileInfo, savesList, type ProfileInfo, type SaveEntry } from "../../api/ps5";
import { consoleAddr } from "../../lib/addr";
import { formatBytes } from "../../lib/format";
import {
  backUpPs4Game,
  ps4BackupFolder,
  ps4Connection,
  ps4Games,
  ps4Users,
  putPs4Save,
  type Ps4Game,
  type Ps4User,
} from "../../lib/ps4Move";
import { safeGetItem, safeSetItem } from "../../lib/safeStorage";
import { useAutoSaveBackupStore } from "../../state/autoSaveBackup";
import { useConnectionStore } from "../../state/connection";
import { useTr } from "../../state/lang";
import { formatAccountId, parseAccountId } from "../Accounts/accountId";
import { userSlots } from "../Accounts/userSlots";

const LAST_PS4 = "ps5upload.ps4_move.host";

type Status = { kind: "idle" } | { kind: "busy"; text: string } | { kind: "error"; text: string };

/**
 * Move saves from a PS4 to this PS5, safely:
 * 1. The PS4 is only read (over its FTP server); nothing is written, renamed or deleted there.
 * 2. Every chosen game's save is copied to this computer first (the save-backups folder) and
 *    checked against the PS4's listing; nothing reaches the PS5 before that.
 * 3. On the PS5 only saves are written: into one user's PS4 save folder of a game, and only a
 *    folder the console already made (the game played and saved there once). The engine refuses
 *    a running game and anything that does not look like a PS4 save, and backs up the PS5's own
 *    save before replacing it. No system files, no folder renames, no trophies.
 * A save follows the account ID: the PS5 user must have the PS4 account's ID.
 */
export default function MoveFromPs4() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const addr = host?.trim() ? consoleAddr(host) : "";
  const dir = useAutoSaveBackupStore((s) => s.settings?.dir ?? "");
  const refreshSettings = useAutoSaveBackupStore((s) => s.refresh);
  const { confirm, dialog } = useConfirm();

  const [open, setOpen] = useState(false);
  const [ps4Host, setPs4Host] = useState(() => safeGetItem(LAST_PS4) ?? "");
  const [port, setPort] = useState("2121");
  const [conn, setConn] = useState<string | null>(null);
  const [users, setUsers] = useState<Ps4User[] | null>(null);
  const [user, setUser] = useState<string>("");
  const [games, setGames] = useState<Ps4Game[] | null>(null);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [status, setStatus] = useState<Status>({ kind: "idle" });
  /** Games copied to this computer: title → the local folder. */
  const [copied, setCopied] = useState<Map<string, string>>(new Map());

  useEffect(() => {
    if (open) void refreshSettings();
  }, [open, refreshSettings]);

  const run = async (text: string, f: () => Promise<void>) => {
    setStatus({ kind: "busy", text });
    try {
      await f();
      setStatus({ kind: "idle" });
    } catch (e) {
      setStatus({ kind: "error", text: e instanceof Error ? e.message : String(e) });
    }
  };

  const connect = () =>
    run(tr("ps4move_connecting", undefined, "Reading the PS4…"), async () => {
      const p = Number(port) || 2121;
      safeSetItem(LAST_PS4, ps4Host.trim());
      const id = await ps4Connection(ps4Host, p);
      setConn(id);
      const u = await ps4Users(id);
      setUsers(u);
      setUser(u[0]?.id ?? "");
      setGames(null);
      setCopied(new Map());
    });

  useEffect(() => {
    if (!conn || !user) return;
    void run(tr("ps4move_reading_games", undefined, "Reading this user's saves…"), async () => {
      const g = await ps4Games(conn, user);
      setGames(g);
      setPicked(new Set(g.map((x) => x.titleId)));
      setCopied(new Map());
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [conn, user]);

  const backUp = () =>
    run(tr("ps4move_copying", undefined, "Copying saves to this computer…"), async () => {
      if (!conn || !games) return;
      const dest = ps4BackupFolder(dir, ps4Host, user, new Date());
      const done = new Map<string, string>();
      for (const g of games.filter((x) => picked.has(x.titleId))) {
        setStatus({
          kind: "busy",
          text: tr("ps4move_copying_one", { title: g.titleId }, `Copying ${g.titleId} to this computer…`),
        });
        done.set(g.titleId, await backUpPs4Game(conn, user, g, dest));
      }
      setCopied(done);
    });

  return (
    <Card className="mb-4" data-testid="ps4-move">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center gap-2 text-start"
        aria-expanded={open}
        data-testid="ps4-move-open"
      >
        <ArrowRightLeft size={16} className="text-[var(--color-accent)]" />
        <span className="flex-1 text-sm font-semibold">
          {tr("ps4move_title", undefined, "Move saves from a PS4")}
          <span className="ms-2 rounded-full bg-[var(--color-surface-2)] px-2 py-0.5 text-[11px] font-normal text-[var(--color-muted)]">
            {tr("ps4move_badge", undefined, "Experimental")}
          </span>
        </span>
      </button>
      {open && (
        <div className="mt-3 space-y-4 text-sm">
          <p className="flex items-start gap-2 text-xs text-[var(--color-muted)]">
            <ShieldCheck size={14} className="mt-px shrink-0" />
            {tr(
              "ps4move_safety",
              undefined,
              "The PS4 is only read, never changed. Its saves are copied to this computer first. On the PS5 only the games you pick are touched, each one's own save is backed up before it is replaced, and nothing else (users, system files, trophies) is changed.",
            )}
          </p>
          <p className="flex items-start gap-2 text-xs text-[var(--color-warn)]" data-testid="ps4-move-experimental">
            <CircleAlert size={14} className="mt-px shrink-0" />
            {tr(
              "ps4move_experimental",
              undefined,
              "Experimental: putting PS4-format saves back on a PS5 has mixed reports, and a game may not load one. Try one game first; its PS5 save is kept, so you can put it back from the game's save backups.",
            )}
          </p>
          {!dir.trim() && (
            <p className="flex items-start gap-2 text-xs text-[var(--color-warn)]" data-testid="ps4-move-no-dir">
              <CircleAlert size={14} className="mt-px shrink-0" />
              {tr(
                "ps4move_need_dir",
                undefined,
                "Choose a folder in Automatic backups above first: the PS4's saves are kept there, and so is each PS5 save before it is replaced.",
              )}
            </p>
          )}

          <section className="space-y-2">
            <h3 className="text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
              {tr("ps4move_step1", undefined, "1. The PS4")}
            </h3>
            <p className="text-xs text-[var(--color-muted)]">
              {tr(
                "ps4move_step1_hint",
                undefined,
                "On the jailbroken PS4, turn on its FTP server (GoldHEN's, usually port 2121), then enter its address.",
              )}
            </p>
            <div className="flex flex-wrap items-end gap-2">
              <Input
                label={tr("ps4move_address", undefined, "PS4 address")}
                value={ps4Host}
                onChange={(e) => setPs4Host(e.target.value)}
                placeholder="192.168.1.50"
                className="w-48"
                data-testid="ps4-move-host"
              />
              <Input
                label={tr("ps4move_port", undefined, "FTP port")}
                value={port}
                onChange={(e) => setPort(e.target.value.replace(/\D/g, ""))}
                className="w-24"
                data-testid="ps4-move-port"
              />
              <Button
                size="sm"
                onClick={() => void connect()}
                disabled={!ps4Host.trim() || status.kind === "busy" || !dir.trim()}
                data-testid="ps4-move-connect"
              >
                {tr("ps4move_connect", undefined, "Read the PS4")}
              </Button>
            </div>
          </section>

          {users && (
            <section className="space-y-2">
              <h3 className="text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
                {tr("ps4move_step2", undefined, "2. Its saves")}
              </h3>
              {users.length === 0 ? (
                <p className="text-xs text-[var(--color-muted)]">
                  {tr("ps4move_no_users", undefined, "No user on this PS4 has saves.")}
                </p>
              ) : (
                <>
                  <Select
                    label={tr("ps4move_user", undefined, "PS4 user (its folder)")}
                    block={false}
                    className="w-64"
                    value={user}
                    onChange={(e) => setUser(e.target.value)}
                    options={users.map((u) => ({
                      value: u.id,
                      label: tr("ps4move_user_row", { id: u.id, n: u.games }, `${u.id} · ${u.games} games`),
                    }))}
                    data-testid="ps4-move-user"
                  />
                  {games && (
                    <ul className="max-h-64 space-y-1 overflow-y-auto" data-testid="ps4-move-games">
                      {games.map((g) => (
                        <li key={g.titleId} className="flex items-center gap-2">
                          <Checkbox
                            checked={picked.has(g.titleId)}
                            onChange={(on) =>
                              setPicked((p) => {
                                const n = new Set(p);
                                if (on) n.add(g.titleId);
                                else n.delete(g.titleId);
                                return n;
                              })
                            }
                            label={`${g.titleId} · ${formatBytes(g.bytes)}`}
                          />
                          {copied.has(g.titleId) && <CheckCircle2 size={14} className="text-[var(--color-good)]" />}
                        </li>
                      ))}
                    </ul>
                  )}
                  <Button
                    size="sm"
                    variant="secondary"
                    onClick={() => void backUp()}
                    disabled={!games || picked.size === 0 || status.kind === "busy"}
                    data-testid="ps4-move-backup"
                  >
                    {tr("ps4move_backup", { n: picked.size }, `Copy ${picked.size} to this computer`)}
                  </Button>
                </>
              )}
            </section>
          )}

          {copied.size > 0 && addr && (
            <PutOnPs5 addr={addr} copied={copied} confirm={confirm} />
          )}

          {status.kind === "busy" && (
            <p className="flex items-center gap-2 text-xs text-[var(--color-muted)]" role="status">
              <Spinner size={12} />
              {status.text}
            </p>
          )}
          {status.kind === "error" && (
            <p className="text-xs text-[var(--color-bad)]" role="alert" data-testid="ps4-move-error">
              {status.text}
            </p>
          )}
        </div>
      )}
      {dialog}
    </Card>
  );
}

/** Step 3: the PS5 user, the account ID check, and which copied games can go on it now. */
function PutOnPs5({
  addr,
  copied,
  confirm,
}: {
  addr: string;
  copied: Map<string, string>;
  confirm: ReturnType<typeof useConfirm>["confirm"];
}) {
  const tr = useTr();
  const [info, setInfo] = useState<ProfileInfo | null>(null);
  const [uid, setUid] = useState<number | null>(null);
  const [saves, setSaves] = useState<SaveEntry[] | null>(null);
  const [ps4Account, setPs4Account] = useState("");
  const [checked, setChecked] = useState(false);
  const [results, setResults] = useState<Map<string, string | true>>(new Map());
  const [busy, setBusy] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    profileInfo(addr)
      .then((i) => {
        if (!live) return;
        setInfo(i);
        setUid(i.uid || i.users[0]?.uid || null);
      })
      .catch(() => live && setInfo(null));
    return () => {
      live = false;
    };
  }, [addr]);

  const reloadSaves = () => {
    if (uid === null) return;
    setSaves(null);
    savesList(addr, uid)
      .then((r) => setSaves(r.saves ?? []))
      .catch(() => setSaves([]));
  };
  useEffect(reloadSaves, [addr, uid]);

  const slot = useMemo(() => {
    if (!info || uid === null) return null;
    return userSlots(info.users, info.slots).get(uid) ?? null;
  }, [info, uid]);
  const ps5Account = slot ? formatAccountId(slot.id) : "—";
  const typed = ps4Account.trim() ? parseAccountId(ps4Account) : null;
  const mismatch = !!typed && typed !== ps5Account;
  const ok = !mismatch && (typed ? typed === ps5Account : checked);

  /** Where each copied game's save goes on this user, or null: no PS4 save of it here yet. */
  const target = (title: string) =>
    saves?.find((s) => s.kind === "ps4" && s.title_id.toUpperCase() === title)?.path ?? null;
  const ready = [...copied.keys()].filter((t) => target(t));

  const put = async () => {
    if (uid === null) return;
    const yes = await confirm({
      title: tr("ps4move_confirm_title", { n: ready.length }, `Put ${ready.length} PS4 saves on this PS5?`),
      message: tr(
        "ps4move_confirm_body",
        undefined,
        "Each game's save on this PS5 user is backed up, then replaced with the PS4's. The games must be closed. You can put a PS5 save back from its backups (marked \"Before a PS4 save\").",
      ),
      confirmLabel: tr("ps4move_put", undefined, "Put on the PS5"),
      destructive: true,
    });
    if (!yes) return;
    const out = new Map<string, string | true>();
    for (const t of ready) {
      setBusy(t);
      try {
        await putPs4Save({ addr, titleId: t, savePath: target(t)!, userId: uid, src: copied.get(t)! });
        out.set(t, true);
      } catch (e) {
        out.set(t, e instanceof Error ? e.message : String(e));
      }
      setResults(new Map(out));
    }
    setBusy(null);
  };

  return (
    <section className="space-y-2" data-testid="ps4-move-ps5">
      <h3 className="text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
        {tr("ps4move_step3", undefined, "3. Onto this PS5")}
      </h3>
      {!info ? (
        <Spinner size={14} />
      ) : (
        <>
          <Select
            label={tr("ps4move_ps5_user", undefined, "PS5 user")}
            block={false}
            className="w-64"
            value={String(uid ?? "")}
            onChange={(e) => setUid(Number(e.target.value))}
            options={info.users.map((u) => ({ value: String(u.uid), label: u.username || u.uid_hex }))}
            data-testid="ps4-move-ps5-user"
          />
          <p className="text-xs">
            {tr("ps4move_ps5_account", { id: ps5Account }, `This user's account ID: ${ps5Account}`)}
          </p>
          <p className="text-xs text-[var(--color-muted)]">
            {tr(
              "ps4move_account_hint",
              undefined,
              "A save follows the account ID: the PS4 account's ID must be this user's. If it is not, set it in Accounts (it changes which saves this user can open) before going on.",
            )}
          </p>
          <Input
            label={tr("ps4move_ps4_account", undefined, "The PS4 account's ID (optional, to check)")}
            value={ps4Account}
            onChange={(e) => setPs4Account(e.target.value)}
            placeholder="0x1234abcd5678ef90"
            className="w-72"
            data-testid="ps4-move-ps4-account"
          />
          {mismatch ? (
            <p className="text-xs text-[var(--color-bad)]" role="alert" data-testid="ps4-move-mismatch">
              {tr(
                "ps4move_mismatch",
                undefined,
                "The IDs differ: the PS4's saves would not open for this user. Give this user the PS4 account's ID in Accounts first, or pick another user.",
              )}
            </p>
          ) : !typed ? (
            <Checkbox
              checked={checked}
              onChange={setChecked}
              label={tr("ps4move_checked", undefined, "I made sure this PS5 user has the PS4 account's ID")}
            />
          ) : (
            <p className="text-xs text-[var(--color-good)]">{tr("ps4move_match", undefined, "The IDs match.")}</p>
          )}
          <ul className="space-y-1" data-testid="ps4-move-targets">
            {[...copied.keys()].map((t) => {
              const r = results.get(t);
              return (
                <li key={t} className="flex flex-wrap items-center gap-2 text-xs">
                  <span className="font-mono">{t}</span>
                  {busy === t ? (
                    <Spinner size={12} />
                  ) : r === true ? (
                    <span className="text-[var(--color-good)]">{tr("ps4move_done", undefined, "Done")}</span>
                  ) : r ? (
                    <span className="text-[var(--color-bad)]">{r}</span>
                  ) : saves === null ? null : target(t) ? (
                    <span className="text-[var(--color-muted)]">{tr("ps4move_ready", undefined, "Ready")}</span>
                  ) : (
                    <span className="text-[var(--color-warn)]">
                      {tr(
                        "ps4move_not_ready",
                        undefined,
                        "Not on this user yet: start the game on the PS5 once and save, then press Check again.",
                      )}
                    </span>
                  )}
                </li>
              );
            })}
          </ul>
          <div className="flex flex-wrap gap-2">
            <Button size="sm" variant="ghost" onClick={reloadSaves} disabled={!!busy}>
              {tr("ps4move_recheck", undefined, "Check again")}
            </Button>
            <Button
              size="sm"
              variant="primary"
              onClick={() => void put()}
              disabled={!ok || ready.length === 0 || !!busy}
              data-testid="ps4-move-put"
            >
              {tr("ps4move_put_n", { n: ready.length }, `Put ${ready.length} on the PS5`)}
            </Button>
          </div>
        </>
      )}
    </section>
  );
}
