import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Eye,
  EyeOff,
  Gamepad2,
  HardDriveDownload,
  Layers,
  Pause,
  Play,
  ShieldCheck,
  Trash2,
} from "lucide-react";

import {
  appdbQuery,
  appResume,
  appSuspend,
  appUnregister,
  fsChmod,
  sdkScan,
  type InstalledTitle,
  type SdkScanResponse,
} from "../../api/ps5";
import { hiddenIcons, setIconHidden } from "../../api/hiddenIcons";
import { Button, Card, ErrorCard } from "../../components";
// Direct import to avoid the barrel's circular-dep warning at build.
import { useConfirm } from "../../components/ConfirmDialog";
import { mgmtAddr, transferAddr } from "../../lib/addr";
import { isBackportEligible } from "../../lib/backport";
import { humanizePs5Error } from "../../lib/humanizeError";
import { useStaleHostGuard } from "../../lib/staleHostGuard";
import { isTauriEnv } from "../../lib/tauriEnv";
import { audit } from "../../state/auditLog";
import { useCollectionStore } from "../../state/collection";
import { useConnectionStore } from "../../state/connection";
import {
  EMPTY_CORPUS,
  loadFakelibCorpus,
  type FakelibCorpus,
} from "../../state/fakelibCorpus";
import { useTr } from "../../state/lang";
import { pushNotification } from "../../state/notifications";
import { withConsolePrefix } from "../../state/roster";
import { BackportPanel } from "../Backport/BackportPanel";
import { DumpThisGame, isDumpable, useTitleDumpJobs } from "./DumpThisGame";
import { useCollectionItems, useCollectionLoaded } from "./collectionLink";
import { InstalledKindBadge, installedKindOf } from "./installedKind";
import { readRunningApps } from "./runningAppsRead";
import { SmpNeededWarning, useSmpState } from "./SmpNeeded";

/** A game installed from a package is gone once uninstalled: say so when the Collection on
 *  this computer (once read) has no copy of it to install again. */
function noCopyWarning(t: InstalledTitle, tr: ReturnType<typeof useTr>): string {
  const library = useCollectionStore.getState().library;
  if (!library?.games || library.games[t.titleId.toUpperCase()]) return "";
  return (
    " " +
    tr(
      "installed_uninstall_no_copy",
      undefined,
      "Your Collection on this computer has no copy of this game: keep its package somewhere before you uninstall.",
    )
  );
}

/**
 * The game as the connected PS5 has it installed: how it got there, and the upkeep that is
 * about this console's copy (hide its icon, fix its folder's permissions, backport it, copy it
 * to the Collection, dump it to a drive, suspend or resume it while it runs, uninstall it).
 * Play and Close game are on its console row above.
 */
export function OnThisPs5Card({
  host,
  consoleName,
  title,
  allTitles,
  running,
  openBackport,
  onLaunch,
  onChanged,
}: {
  /** The connected console (its helper answers). */
  host: string;
  consoleName: string;
  title: InstalledTitle;
  /** Every title the console has installed: what a backport's library scan may walk. */
  allTitles: InstalledTitle[];
  running: boolean;
  /** Open the backport panel once (the Collection's "Backport…" lands here with it). */
  openBackport?: boolean;
  onLaunch: () => void;
  /** After an uninstall: read the console again. */
  onChanged: () => void;
}) {
  const tr = useTr();
  const guard = useStaleHostGuard();
  const { confirm, dialog } = useConfirm();
  const kind = installedKindOf(title);
  const smp = useSmpState(kind === "disc" ? host : "");
  const ps5Kernel = useConnectionStore((s) => s.ps5Kernel);
  const [busy, setBusy] = useState<string | null>(null);
  const [actionError, setActionError] = useState<{ title: string; detail: string } | null>(null);
  /** Whether its home-screen icon is hidden; undefined when the console cannot say. */
  const [hidden, setHidden] = useState<boolean | undefined>(undefined);
  const [sdk, setSdk] = useState<SdkScanResponse>({ titles: [] });
  const [backportOpen, setBackportOpen] = useState(false);
  const [corpus, setCorpus] = useState<FakelibCorpus>(EMPTY_CORPUS);
  const [dumpOpen, setDumpOpen] = useState(false);
  const dumpable = isDumpable(title);
  // One dump of a game at a time: while one is queued or running, its row says so.
  const dumping = useTitleDumpJobs(host, title.titleId).some(
    (j) => j.state === "waiting" || j.state === "running",
  );
  useCollectionLoaded();
  const copyItems = useCollectionItems(host, title, { copyOnly: true });

  // What the console says about the icon and the executable's SDK: best-effort, each.
  useEffect(() => {
    let cancelled = false;
    setHidden(undefined);
    setSdk({ titles: [] });
    hiddenIcons(host)
      .then((ids) => {
        if (!cancelled) setHidden(ids.includes(title.titleId));
      })
      .catch(() => {});
    if (title.source) {
      sdkScan(mgmtAddr(host))
        .then((r) => {
          if (!cancelled) setSdk(r);
        })
        .catch(() => {});
    }
    return () => {
      cancelled = true;
    };
  }, [host, title.titleId, title.source]);

  const sdkRow = useMemo(
    () => (sdk.titles ?? []).find((r) => r.title_id === title.titleId),
    [sdk, title.titleId],
  );
  const backportEligible = isBackportEligible(title, sdkRow, ps5Kernel);

  // Handed over for backporting: open the panel once.
  const handed = useRef(false);
  useEffect(() => {
    if (openBackport && !handed.current) {
      handed.current = true;
      setBackportOpen(true);
    }
  }, [openBackport]);
  // The corpus is the user's own libraries; read again whenever the panel opens.
  const refreshCorpus = useCallback(() => loadFakelibCorpus().then(setCorpus), []);
  useEffect(() => {
    if (backportOpen) void refreshCorpus();
  }, [backportOpen, refreshCorpus]);
  const scanTitles = useMemo(
    () =>
      allTitles
        .filter((t) => !!t.source && !t.system)
        .map((t) => ({
          title_id: t.titleId,
          title_name: t.titleName,
          image_backed: t.imageBacked,
          source: t.source,
        })),
    [allTitles],
  );

  const uninstall = async () => {
    const probe = guard.capture();
    const t = title;
    const ok = await confirm({
      title: tr(
        "installed_uninstall_confirm_title",
        { name: t.titleName },
        `Uninstall ${t.titleName}?`,
      ),
      message: t.system
        ? tr(
            "installed_uninstall_confirm_system",
            { id: t.titleId },
            `${t.titleId} is a SYSTEM app. Removing it can destabilize the console and may require a reinstall to recover. Only continue if you know exactly what this package is.`,
          )
        : t.origin === "registered"
          ? tr(
              "installed_uninstall_confirm_registered",
              undefined,
              "This unmounts and removes the title from the home screen. Your source files/image on disk are not deleted.",
            )
          : tr(
              "installed_uninstall_confirm_pkg",
              undefined,
              "This removes the installed title from the PS5. You can reinstall it later from the package.",
            ) + noCopyWarning(t, tr),
      confirmLabel: tr("installed_uninstall", undefined, "Uninstall"),
      destructive: true,
    });
    if (!ok || probe.isStale()) return;
    setBusy("uninstall");
    setActionError(null);
    try {
      const res = await appUnregister(transferAddr(probe.host), t.titleId);
      audit("app_uninstall", withConsolePrefix(probe.host, t.titleName), {
        context: t.titleId,
      });
      if (probe.isStale()) return;
      if (res.stillPresent) {
        // #476: the PS5 accepted, but the game is still there: say so rather than drop it.
        pushNotification("warning", withConsolePrefix(probe.host, t.titleName), {
          body: tr(
            "installed_uninstall_still_present",
            { name: t.titleName },
            `The PS5 accepted the uninstall, but ${t.titleName} is still on the console. A large game can take a few minutes to remove: refresh in a moment. If it stays, delete it from the PS5's Settings → Storage.`,
          ),
        });
      }
      onChanged();
    } catch (e) {
      if (probe.isStale()) return;
      const raw = e instanceof Error ? e.message : String(e);
      // Sony's 0x80B2xxxx codes read as install failures in the shared wording: an uninstall
      // says what it is.
      const code = /0x80B2_?[0-9A-Fa-f]{4}/i.exec(raw)?.[0];
      setActionError({
        title: tr(
          "installed_uninstall_failed",
          { name: t.titleName },
          `Couldn't uninstall ${t.titleName}`,
        ),
        detail: code
          ? tr(
              "installed_uninstall_refused",
              { code },
              `The PS5 refused to uninstall it (${code}). Close the game if it is running and try again; if it stays, delete it from the PS5's Settings → Storage.`,
            )
          : humanizePs5Error(raw),
      });
    } finally {
      setBusy(null);
    }
  };

  /** Hide or show the home-screen icon (#407): only app.db's visible flag changes. */
  const toggleHidden = async () => {
    if (hidden === undefined) return;
    const probe = guard.capture();
    const hide = !hidden;
    setBusy("hide");
    setActionError(null);
    try {
      await setIconHidden(probe.host, title.titleId, hide);
      audit(hide ? "app_icon_hide" : "app_icon_show", withConsolePrefix(probe.host, title.titleName), {
        context: title.titleId,
      });
      if (probe.isStale()) return;
      setHidden(hide);
      // No question first: this undoes from the same menu. It still says what it did and did not do.
      if (hide) {
        pushNotification("success", withConsolePrefix(probe.host, title.titleName), {
          body: tr(
            "installed_hide_confirm_body",
            undefined,
            "Only its icon is hidden: it stays installed, keeps its saves, and you can still start it from here. Show it again from this menu. The PS5's home screen updates the next time you open and close any app.",
          ),
        });
      }
    } catch (e) {
      if (probe.isStale()) return;
      const raw = e instanceof Error ? e.message : String(e);
      setActionError({
        title: hide
          ? tr("installed_hide_failed", { name: title.titleName }, `Couldn't hide ${title.titleName}`)
          : tr("installed_show_failed", { name: title.titleName }, `Couldn't show ${title.titleName}`),
        detail: humanizePs5Error(raw),
      });
    } finally {
      setBusy(null);
    }
  };

  const fixPermissions = async () => {
    if (!title.source) return;
    const probe = guard.capture();
    setBusy("chmod");
    try {
      await fsChmod(transferAddr(probe.host), title.source, "0777", true);
      if (probe.isStale()) return;
      pushNotification("success", withConsolePrefix(probe.host, title.titleName), {
        body: tr(
          "installed_fix_permissions_done",
          undefined,
          "Permissions fixed: every file in this game's folder is now 0777. Press Play.",
        ),
      });
    } catch (e) {
      if (probe.isStale()) return;
      const raw = e instanceof Error ? e.message : String(e);
      pushNotification("error", withConsolePrefix(probe.host, title.titleName), {
        body: humanizePs5Error(raw),
      });
    } finally {
      setBusy(null);
    }
  };

  /** Suspend or resume the running game: find its app id, then ask. */
  const lifecycle = async (which: "suspend" | "resume") => {
    const addr = mgmtAddr(host);
    const label =
      which === "suspend"
        ? tr("running_apps_suspend", undefined, "Suspend")
        : tr("running_apps_resume", undefined, "Resume");
    setBusy(which);
    try {
      const { rows } = await readRunningApps(
        addr,
        isTauriEnv() ? () => appdbQuery(addr).then((r) => r.apps) : null,
      );
      const row = rows.find((r) => r.title_id?.toUpperCase() === title.titleId.toUpperCase());
      if (!row) throw new Error(tr("game_ps5_not_running", undefined, "The PS5 does not list this game as running."));
      const ack = await (which === "suspend" ? appSuspend : appResume)(addr, row.app_id);
      if (!ack.ok) throw new Error(ack.err ?? "unknown error");
      pushNotification("info", withConsolePrefix(host, `${label}: ${title.titleName}`));
    } catch (e) {
      pushNotification("error", withConsolePrefix(host, `${label}: ${title.titleName}`), {
        body: humanizePs5Error(e instanceof Error ? e.message : String(e)),
      });
    } finally {
      setBusy(null);
    }
  };

  return (
    <Card>
      {dialog}
      {backportOpen && (
        <BackportPanel
          open
          host={host}
          title={title}
          sets={corpus.sets}
          corpusRoot={corpus.root}
          onCorpusChanged={() => void refreshCorpus()}
          scanTitles={scanTitles}
          scan={sdk}
          onClose={() => setBackportOpen(false)}
          onLaunch={() => onLaunch()}
        />
      )}
      <div className="mb-4 flex flex-wrap items-center gap-x-3 gap-y-1" data-testid="game-on-this-ps5">
        <h2 className="flex items-center gap-3 text-[1.0625rem] font-semibold tracking-[-0.01em]">
          <span className="grid h-8 w-8 shrink-0 place-items-center rounded-full bg-[var(--color-accent-soft)] text-[var(--color-accent-bright)]">
            <Gamepad2 size={15} aria-hidden />
          </span>
          {tr("game_tab_on", { name: consoleName }, "On {name}")}
        </h2>
        <InstalledKindBadge title={title} />
        {hidden && (
          <span
            className="inline-flex items-center gap-1 text-xs text-[var(--color-muted)]"
            title={tr(
              "installed_hidden_tooltip",
              undefined,
              "Hidden from the PS5 home screen. It is still installed; show it again from ⋯.",
            )}
          >
            <EyeOff size={11} aria-hidden />
            {tr("installed_hidden_badge", undefined, "Hidden")}
          </span>
        )}
      </div>
      {title.source && (
        <p className="mb-3 truncate font-mono text-xs text-[var(--color-muted)]" title={title.source}>
          {title.source}
        </p>
      )}
      {kind === "disc" && smp.absent && (
        <div className="mb-3">
          <SmpNeededWarning host={host} count={1} />
        </div>
      )}
      {actionError && (
        <div className="mb-3">
          <ErrorCard title={actionError.title} detail={actionError.detail} />
        </div>
      )}
      <div className="flex flex-wrap items-center gap-2">
        {running && (
          <>
            <Button
              size="sm"
              variant="secondary"
              leftIcon={<Pause size={13} />}
              loading={busy === "suspend"}
              disabled={busy !== null}
              onClick={() => void lifecycle("suspend")}
            >
              {tr("running_apps_suspend", undefined, "Suspend")}
            </Button>
            <Button
              size="sm"
              variant="secondary"
              leftIcon={<Play size={13} />}
              loading={busy === "resume"}
              disabled={busy !== null}
              onClick={() => void lifecycle("resume")}
            >
              {tr("running_apps_resume", undefined, "Resume")}
            </Button>
          </>
        )}
        {backportEligible && (
          <Button
            size="sm"
            variant="secondary"
            leftIcon={<Layers size={13} />}
            onClick={() => setBackportOpen(true)}
          >
            {tr("backport_action", undefined, "Backport")}
          </Button>
        )}
        {copyItems.map((it) => (
          <Button
            key={it.label}
            size="sm"
            variant="secondary"
            leftIcon={it.icon}
            title={it.title}
            onClick={it.onSelect}
          >
            {it.label}
          </Button>
        ))}
        {dumpable && (
          <Button
            size="sm"
            variant="secondary"
            leftIcon={<HardDriveDownload size={13} />}
            aria-expanded={dumpOpen}
            disabled={dumping}
            title={tr(
              "dump_this_game_hint",
              undefined,
              "Copy this game's files to a USB drive or the console, as a plain copy or with its executables decrypted.",
            )}
            onClick={() => setDumpOpen((o) => !o)}
            data-testid="dump-this-game"
          >
            {tr("dump_this_game", undefined, "Dump this game")}
          </Button>
        )}
        {hidden !== undefined && !title.system && (
          <Button
            size="sm"
            variant="ghost"
            leftIcon={hidden ? <Eye size={13} /> : <EyeOff size={13} />}
            loading={busy === "hide"}
            disabled={busy !== null}
            onClick={() => void toggleHidden()}
          >
            {hidden
              ? tr("installed_show_on_home", undefined, "Show on home screen")
              : tr("installed_hide_from_home", undefined, "Hide from home screen")}
          </Button>
        )}
        {title.source && (
          <Button
            size="sm"
            variant="ghost"
            leftIcon={<ShieldCheck size={13} />}
            loading={busy === "chmod"}
            disabled={busy !== null}
            title={tr(
              "installed_fix_permissions",
              undefined,
              "Fix permissions: make this game's folder readable and runnable by the PS5 (chmod 777)",
            )}
            onClick={() => void fixPermissions()}
          >
            {tr("installed_fix_permissions_short", undefined, "Fix permissions")}
          </Button>
        )}
        <Button
          size="sm"
          variant="danger"
          leftIcon={<Trash2 size={13} />}
          loading={busy === "uninstall"}
          disabled={busy !== null}
          onClick={() => void uninstall()}
        >
          {tr("installed_uninstall", undefined, "Uninstall")}
        </Button>
      </div>
      {dumpable && (
        <DumpThisGame
          host={host}
          title={title}
          open={dumpOpen && !dumping}
          onStarted={() => setDumpOpen(false)}
        />
      )}
    </Card>
  );
}
