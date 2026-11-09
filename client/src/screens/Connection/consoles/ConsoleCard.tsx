// One console's card on the Consoles page: how it is at a glance, the ways to reach it (home
// network, Tailscale, internet later), its actions, the setup checklist while it is not ready,
// and who may use it.

import { useEffect, useState, type ReactNode } from "react";
import { useNavigate } from "react-router";
import {
  Check,
  ChevronDown,
  ChevronRight,
  Cloud,
  Globe,
  Home as HomeIcon,
  Pencil,
  Power,
  Send,
  Settings2,
  UserPlus,
  Users,
  X,
} from "lucide-react";

import { Badge, Button, OverflowMenu } from "../../../components";
import { useConfirm, usePrompt } from "../../../components/ConfirmDialog";
import { useTr, type Translator } from "../../../state/lang";
import { useHostRuntime, useConnectionStore } from "../../../state/connection";
import { useRosterStore, type PS5Profile } from "../../../state/roster";
import { helperSendFor, useHelperSendStore } from "../../../state/helperSend";
import { useHelperStopped } from "../../../state/helperStopped";
import { sendHelperTo } from "../../../state/helperSendRuntime";
import { meshFor, pollMesh, useMeshStore } from "../../../state/mesh";
import { consoleOnTailnet } from "../../../state/useTailnet";
import { useConsoleWake } from "../../../lib/useConsoleWake";
import { cleanConsoleAddress, consoleAddr, hostOf } from "../../../lib/addr";
import { ps5SettingsLink } from "../../Settings/settingsTabs";
import {
  allowList,
  tailnetOp,
  tailnetStatus,
  TailnetUnsupportedError,
  type AppTailnetStatus,
  type TailnetStatus,
} from "../../../api/tailnet";
import type { MeshState } from "../../../api/mesh";
import { humanizeCaught } from "../../../lib/humanizeError";
import { StopHelperButton } from "../StopHelper";
import { useHelperReplaceGuard } from "../useHelperReplaceGuard";
import { OnPausedSwitch } from "../TailnetBits";
import TailnetPanel from "../TailnetPanel";
import { cardStatus, isTailnetIp, showChecklist, type CardStatus } from "./model";
import { SetupChecklist } from "./SetupChecklist";
import { ElfldrFirstCallout } from "./setupBits";
import { TrustedDevices, AskRows } from "./TrustedDevices";
import { InviteSheet } from "./InviteSheet";
import { meshDevice } from "../../../api/mesh";
import { resetMeshCursor } from "../../../state/mesh";
import { consoleBusy } from "../../../lib/usePoll";

// ---- words ----

export function statusLine(tr: Translator, s: CardStatus): string {
  if (s.power === "rest") return tr("cs_card_rest", undefined, "In rest mode");
  if (s.power === "offline") return tr("cs_card_offline", undefined, "Not reachable");
  if (s.power === "unknown") return tr("cs_card_checking", undefined, "Checking…");
  if (s.path.kind === "tailscale") {
    return s.path.via === "direct"
      ? tr("cs_card_via_tailscale_direct", { ip: s.path.addr }, "Online · Tailscale {ip}, direct")
      : s.path.via === "system"
        ? tr("cs_card_via_tailscale_system", { ip: s.path.addr }, "Online · Tailscale {ip}, through the Tailscale app")
        : tr("cs_card_via_tailscale_relayed", { ip: s.path.addr }, "Online · Tailscale {ip}, relayed");
  }
  return tr("cs_card_via_home", { ip: s.path.kind === "home" ? s.path.addr : "" }, "Online · Home network {ip}");
}

function helperChip(tr: Translator, s: CardStatus): { text: string; tone: "good" | "warn" | "neutral" | "accent" } {
  const v = s.helperVersion ? `v${s.helperVersion}` : "";
  switch (s.helper) {
    case "match":
      return { text: tr("cs_card_helper_match", { v }, "Helper {v} · matches this app"), tone: "good" };
    case "update":
      return { text: tr("cs_card_helper_update", { v }, "Helper {v} · update available"), tone: "warn" };
    case "newer":
      return { text: tr("cs_card_helper_newer", { v }, "Helper {v} · newer than this app"), tone: "neutral" };
    case "sending":
      return { text: tr("cs_card_helper_sending", undefined, "Sending the helper…"), tone: "accent" };
    case "pairing":
      return { text: tr("cs_card_helper_pairing", undefined, "Helper runs · waiting to let this device in"), tone: "warn" };
    case "stopped":
      return { text: tr("cs_card_helper_stopped", undefined, "Helper stopped by you"), tone: "neutral" };
    case "missing":
      return { text: tr("cs_card_helper_missing", undefined, "Helper not running"), tone: "warn" };
    default:
      return { text: tr("cs_card_helper_unknown", undefined, "Checking the helper…"), tone: "neutral" };
  }
}

function dotClass(s: CardStatus): string {
  if (s.power === "online") return s.ready ? "bg-[var(--color-good)]" : "bg-[var(--color-warn)]";
  if (s.power === "rest") return "bg-[var(--color-accent-bright)]";
  return "bg-[var(--color-muted)] opacity-60";
}

// ---- the ways to reach it ----

function ReachRow({
  icon,
  label,
  value,
  note,
  control,
  muted = false,
  children,
  testId,
}: {
  icon: ReactNode;
  label: string;
  value?: ReactNode;
  note?: ReactNode;
  control?: ReactNode;
  muted?: boolean;
  children?: ReactNode;
  testId: string;
}) {
  return (
    <li className={`py-2.5 ${muted ? "opacity-70" : ""}`} data-testid={testId}>
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5">
        <span className="grid h-8 w-8 shrink-0 place-items-center rounded-full bg-[var(--color-surface-3)] text-[var(--color-muted)]" aria-hidden>
          {icon}
        </span>
        <div className="min-w-0 flex-1 basis-40">
          <div className="text-xs font-medium text-[var(--color-muted)]">{label}</div>
          {value ? <div className="min-w-0 break-words text-sm">{value}</div> : null}
        </div>
        {control ? <div className="flex shrink-0 flex-wrap items-center gap-2">{control}</div> : null}
      </div>
      {note ? <div className="mt-1 ps-11 text-xs text-[var(--color-muted)]">{note}</div> : null}
      {children ? <div className="mt-2 ps-0 sm:ps-11">{children}</div> : null}
    </li>
  );
}

/** The home address, editable in place. Always tried first. */
function HomeAddress({
  host,
  inUse,
  onSave,
}: {
  host: string;
  inUse: boolean;
  onSave: (v: string) => void;
}) {
  const tr = useTr();
  /** Kept at its Tailscale address (the engine had no home address for it): no home one yet. */
  const homeUnknown = isTailnetIp(host);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(homeUnknown ? "" : host);
  useEffect(() => setDraft(isTailnetIp(host) ? "" : host), [host]);
  const save = () => {
    const v = cleanConsoleAddress(draft);
    setEditing(false);
    if (v && v !== hostOf(host)) onSave(v);
  };
  return (
    <ReachRow
      icon={<HomeIcon size={15} />}
      label={tr("cs_reach_home", undefined, "Home network")}
      testId="consoles-reach-home"
      value={
        editing ? (
          <form
            className="flex min-w-0 items-center gap-1.5"
            onSubmit={(e) => {
              e.preventDefault();
              save();
            }}
          >
            <input
              className="input min-w-0 flex-1 font-mono max-md:min-h-10"
              value={draft}
              autoFocus
              onChange={(e) => setDraft(e.target.value)}
              placeholder="192.168.1.50"
              inputMode="url"
              autoCapitalize="off"
              autoCorrect="off"
              spellCheck={false}
              aria-label={tr("ps5_address", undefined, "PS5 IP address")}
              data-testid="consoles-home-input"
            />
            <Button type="submit" size="sm" variant="primary" aria-label={tr("save", undefined, "Save")}>
              <Check size={13} />
            </Button>
            <Button
              size="sm"
              variant="ghost"
              aria-label={tr("cancel", undefined, "Cancel")}
              onClick={() => {
                setDraft(homeUnknown ? "" : host);
                setEditing(false);
              }}
            >
              <X size={13} />
            </Button>
          </form>
        ) : homeUnknown ? (
          <span className="text-[var(--color-muted)]" data-testid="consoles-home-unknown">
            {tr("cs_reach_home_unknown", undefined, "Not known yet")}
          </span>
        ) : (
          <span className="font-mono">{hostOf(host) || "—"}</span>
        )
      }
      note={
        homeUnknown
          ? tr(
              "cs_reach_home_unknown_note",
              undefined,
              "Added over Tailscale. Press Edit to type its home address (the PS5's network settings show it), or it is filled in when this device finds the PS5 at home.",
            )
          : tr("cs_reach_home_note", undefined, "Always tried first.")
      }
      control={
        editing ? null : (
          <>
            {inUse && !homeUnknown ? (
              <Badge tone="good" dot>
                {tr("cs_reach_in_use", undefined, "In use")}
              </Badge>
            ) : null}
            <Button
              size="sm"
              variant="ghost"
              leftIcon={<Pencil size={12} />}
              onClick={() => setEditing(true)}
              data-testid="consoles-home-edit"
            >
              {tr("cs_reach_edit", undefined, "Edit")}
            </Button>
          </>
        )
      }
    />
  );
}

/** What the Tailscale row knows: the console's own Tailscale (when its helper answers) and this
 *  device's view of it. */
export interface TailnetSummary {
  /** The helper answered the Tailscale question (an older helper has no Tailscale). */
  known: boolean;
  setUp: boolean;
  on: boolean;
  connecting: boolean;
  ip: string | null;
  /** When "Try Tailscale now" last reached it (ms; 0: never). */
  testedAt: number;
  allowMode: "listed" | "all" | null;
  listed: number;
  inUse: boolean;
}

export function tailnetSummary(st: TailnetStatus | null, app: AppTailnetStatus | null, host: string): TailnetSummary {
  const fromApp = consoleOnTailnet(app, host);
  const testedAt = app?.consoles?.find((c) => consoleAddr(c.host) === consoleAddr(host))?.tested_at ?? 0;
  return {
    known: !!st,
    setUp: !!st && (st.enabled || !!st.signed_in),
    on: !!st?.enabled,
    connecting: !!st?.enabled && st.state !== "running",
    ip: (st?.state === "running" && st.ip) || fromApp?.ip || null,
    testedAt,
    allowMode: st?.allow_mode ?? null,
    listed: allowList(st).length,
    inUse: !!fromApp && fromApp.via !== "lan",
  };
}

function TailscaleRow({
  tn,
  busy,
  open,
  onToggleOpen,
  onSwitch,
  onTest,
  panel,
}: {
  tn: TailnetSummary;
  busy: boolean;
  open: boolean;
  onToggleOpen: () => void;
  onSwitch: (on: boolean) => void;
  onTest: () => void;
  panel: ReactNode;
}) {
  const tr = useTr();
  const who =
    tn.allowMode === "all"
      ? tr("cs_reach_ts_who_all", undefined, "Every device on your Tailscale network")
      : tn.allowMode === "listed"
        ? tr("cs_reach_ts_who_listed", { count: tn.listed }, "{count} devices you ticked")
        : null;
  return (
    <ReachRow
      icon={<Globe size={15} />}
      label={tr("cs_reach_tailscale", undefined, "Tailscale")}
      testId="consoles-reach-tailscale"
      value={
        tn.ip ? (
          <span className="font-mono">{tn.ip}</span>
        ) : (
          <span className="text-[var(--color-muted)]">
            {tn.setUp ? tr("cs_reach_ts_connecting", undefined, "Connecting…") : tr("cs_reach_ts_off", undefined, "Not set up")}
          </span>
        )
      }
      note={
        tn.setUp && tn.on ? (
          <span className="flex flex-wrap items-center gap-x-3 gap-y-0.5">
            {tn.testedAt > 0 ? (
              <span className="inline-flex items-center gap-1 text-[var(--color-good)]" data-testid="consoles-ts-tested">
                <Check size={12} aria-hidden />
                {tr("cs_reach_ts_tested", undefined, "Tested: reached through Tailscale")}
              </span>
            ) : (
              <span>{tr("cs_reach_ts_untested", undefined, "Not tested yet")}</span>
            )}
            {who ? (
              <span>
                {tr("cs_reach_ts_who", undefined, "Who can reach it:")} {who}
              </span>
            ) : null}
          </span>
        ) : !tn.setUp ? (
          tr("cs_reach_ts_hint", undefined, "Play and upload from anywhere. Set it up once, at home.")
        ) : null
      }
      control={
        <>
          {tn.inUse ? (
            <Badge tone="good" dot>
              {tr("cs_reach_in_use", undefined, "In use")}
            </Badge>
          ) : null}
          {tn.setUp ? (
            <OnPausedSwitch on={tn.on} connecting={tn.connecting} busy={busy} onChange={onSwitch} testId="consoles-ts-switch" />
          ) : null}
          {tn.setUp && tn.on && tn.ip ? (
            <Button size="sm" variant="ghost" disabled={busy} onClick={onTest} data-testid="consoles-ts-test">
              {tr("cs_reach_ts_test", undefined, "Test")}
            </Button>
          ) : null}
          {tn.known ? (
            <Button
              size="sm"
              variant="ghost"
              aria-expanded={open}
              rightIcon={open ? <ChevronDown size={12} /> : <ChevronRight size={12} className="rtl:rotate-180" />}
              onClick={onToggleOpen}
              data-testid="consoles-ts-details"
            >
              {tn.setUp ? tr("cs_reach_ts_details", undefined, "Details") : tr("cs_reach_ts_set_up", undefined, "Set up")}
            </Button>
          ) : null}
        </>
      }
    >
      {open ? panel : null}
    </ReachRow>
  );
}

function InternetRow() {
  const tr = useTr();
  return (
    <ReachRow
      icon={<Cloud size={15} />}
      label={tr("cs_reach_internet", undefined, "Internet")}
      testId="consoles-reach-internet"
      muted
      value={<span className="text-[var(--color-muted)]">{tr("cs_reach_internet_value", undefined, "Not set up yet. Coming in a later version")}</span>}
      note={tr(
        "cs_reach_internet_note",
        undefined,
        "Without Tailscale: a PS5Upload left running at home would pass the connection on.",
      )}
      control={
        <Button size="sm" variant="ghost" disabled aria-disabled>
          {tr("cs_reach_internet_soon", undefined, "Later")}
        </Button>
      }
    />
  );
}

// ---- the card ----

export interface ConsoleCardViewProps {
  profile: PS5Profile;
  active: boolean;
  status: CardStatus;
  tn: TailnetSummary;
  mesh: MeshState | null;
  canWake: boolean;
  wakeBusy: boolean;
  wakeError: string | null;
  tsBusy: boolean;
  tsOpen: boolean;
  devicesOpen: boolean;
  highlight?: boolean;
  error: string | null;
  /** The helper before this one did not end cleanly (another payload ended it, low memory). */
  priorKilled?: boolean;
  checklist: ReactNode;
  tailnetPanel: ReactNode;
  trusted: ReactNode;
  stop: ReactNode;
  onUse: () => void;
  onResend: () => void;
  onWake: () => void;
  onSettings: () => void;
  onInvite: () => void;
  onRename: () => void;
  onNotes: () => void;
  onRemove: () => void;
  onSaveHome: (v: string) => void;
  onTsSwitch: (on: boolean) => void;
  onTsTest: () => void;
  onToggleTs: () => void;
  onToggleDevices: () => void;
  onAnswer: (op: "allow" | "decline", id: string) => void;
  /** An Allow / Decline is on its way: the buttons wait for it. */
  answerBusy?: boolean;
}

export function ConsoleCardView(p: ConsoleCardViewProps) {
  const tr = useTr();
  const s = p.status;
  const chip = helperChip(tr, s);
  const asks = p.mesh?.asks ?? [];
  const trustedCount = p.mesh ? p.mesh.devices.filter((d) => !d.blocked).length : null;
  const up = s.helper === "match" || s.helper === "update" || s.helper === "newer";
  return (
    <article
      className={`rounded-[var(--radius-panel)] border bg-[var(--color-surface-raised)] p-4 shadow-[var(--edge-highlight),var(--shadow-1)] sm:p-5 ${
        p.highlight ? "border-[color-mix(in_srgb,var(--color-accent)_55%,transparent)]" : "border-[var(--glass-edge)]"
      }`}
      aria-labelledby={`console-${p.profile.id}-name`}
      data-testid="console-card"
      data-console={p.profile.id}
      data-ready={s.ready ? "1" : "0"}
    >
      <header className="flex items-start gap-3">
        <span className={`mt-2 h-2.5 w-2.5 shrink-0 rounded-full ${dotClass(s)}`} aria-hidden />
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <h2 id={`console-${p.profile.id}-name`} className="truncate text-base font-semibold">
              {p.profile.name}
            </h2>
            {p.active ? <Badge tone="accent">{tr("cs_card_selected", undefined, "Selected")}</Badge> : null}
          </div>
          <p className="mt-0.5 break-words text-sm text-[var(--color-muted)]" data-testid="console-card-status">
            {statusLine(tr, s)}
          </p>
        </div>
        <OverflowMenu
          ariaLabel={tr("cs_card_more", { name: p.profile.name }, "More for {name}")}
          items={[
            ...(p.active ? [] : [{ label: tr("cs_card_use", undefined, "Use this PS5"), onSelect: p.onUse }]),
            { label: tr("cs_card_rename", undefined, "Rename"), onSelect: p.onRename, separator: !p.active },
            { label: tr("cs_card_notes", undefined, "Notes"), onSelect: p.onNotes },
            { label: tr("remove", undefined, "Remove"), onSelect: p.onRemove, destructive: true, separator: true },
          ]}
        />
      </header>

      <div className="mt-3 flex flex-wrap gap-1.5" data-testid="console-card-facts">
        <Badge tone={chip.tone} size="md">
          {chip.text}
        </Badge>
        {s.firmware ? (
          <Badge tone="neutral" size="md">
            {tr("cs_card_firmware", { fw: s.firmware }, "Firmware {fw}")}
          </Badge>
        ) : null}
        {up && s.kernelRw !== null ? (
          <Badge tone={s.kernelRw ? "good" : "warn"} size="md">
            {s.kernelRw
              ? tr("cs_card_kernel_yes", undefined, "Kernel access")
              : tr("cs_card_kernel_no", undefined, "No kernel access: installs fail")}
          </Badge>
        ) : null}
      </div>

      {p.priorKilled ? (
        <div className="mt-3 flex flex-col gap-2 text-xs" data-testid="console-card-prior-killed">
          <p className="text-[var(--color-warn)]">
            <span className="font-medium">
              {tr("connection_prior_killed", undefined, "The previous helper did not shut down cleanly")}
            </span>
            {". "}
            {tr(
              "connection_prior_killed_detail",
              undefined,
              "This can mean another payload on the console ended it, the system ran low on memory, or it crashed. stderr.log in a bug report says which. If you load ps5upload through an autoloader, try sending it from here instead.",
            )}
          </p>
          <ElfldrFirstCallout />
        </div>
      ) : null}

      {p.profile.notes ? (
        <p className="mt-2 whitespace-pre-wrap text-xs italic text-[var(--color-muted)]">{p.profile.notes}</p>
      ) : null}

      {/* With the device list open its own rows answer the ask: shown once, not twice. */}
      {asks.length > 0 && !p.devicesOpen ? (
        <div className="mt-3">
          <AskRows asks={asks} consoleName={p.profile.name} busy={!!p.answerBusy} onAnswer={p.onAnswer} />
        </div>
      ) : null}

      {showChecklist(s) ? <div className="mt-4">{p.checklist}</div> : null}

      <section className="mt-4" aria-label={tr("cs_reach_title", undefined, "Ways to reach it")}>
        <h3 className="text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
          {tr("cs_reach_title", undefined, "Ways to reach it")}
        </h3>
        <ul className="mt-1 divide-y divide-[var(--color-border)]">
          <HomeAddress host={p.profile.host} inUse={s.path.kind === "home"} onSave={p.onSaveHome} />
          <TailscaleRow
            tn={p.tn}
            busy={p.tsBusy}
            open={p.tsOpen}
            onToggleOpen={p.onToggleTs}
            onSwitch={p.onTsSwitch}
            onTest={p.onTsTest}
            panel={p.tailnetPanel}
          />
          <InternetRow />
        </ul>
      </section>

      <div className="mt-4 flex flex-wrap items-start gap-2 border-t border-[var(--color-border)] pt-4" data-testid="console-card-actions">
        {s.ready ? (
          <Button size="sm" variant="primary" leftIcon={<UserPlus size={13} />} onClick={p.onInvite} data-testid="consoles-invite-open">
            {tr("cs_card_invite", undefined, "Invite a device")}
          </Button>
        ) : null}
        {up || s.helper === "pairing" ? (
          <Button size="sm" variant="secondary" leftIcon={<Send size={13} />} onClick={p.onResend} data-testid="consoles-resend">
            {s.helper === "update"
              ? tr("connection_payload_outdated_action", undefined, "Replace helper")
              : tr("connection_send_resend", undefined, "Resend helper")}
          </Button>
        ) : null}
        {up ? p.stop : null}
        {(s.power === "rest" || s.power === "offline") && p.canWake ? (
          <Button size="sm" variant="secondary" leftIcon={<Power size={13} />} loading={p.wakeBusy} onClick={p.onWake} data-testid="consoles-wake">
            {tr("cs_card_wake", undefined, "Wake")}
          </Button>
        ) : null}
        <Button size="sm" variant="ghost" leftIcon={<Settings2 size={13} />} onClick={p.onSettings}>
          {tr("connection_ps5_settings_open", undefined, "Settings for this PS5")}
        </Button>
        {!p.active ? (
          <Button size="sm" variant="ghost" onClick={p.onUse} data-testid="consoles-use">
            {tr("cs_card_use", undefined, "Use this PS5")}
          </Button>
        ) : null}
      </div>
      {p.wakeError ? (
        <p role="alert" className="mt-2 text-xs text-[var(--color-bad)]">
          {p.wakeError}
        </p>
      ) : null}
      {p.error ? (
        <p role="alert" className="mt-2 text-xs text-[var(--color-bad)]">
          {p.error}
        </p>
      ) : null}

      <div className="mt-3">
        <button
          type="button"
          className="flex w-full items-center gap-2 rounded-[var(--radius-field)] py-1.5 text-start text-sm font-medium hover:text-[var(--color-accent-bright)]"
          aria-expanded={p.devicesOpen}
          onClick={p.onToggleDevices}
          data-testid="consoles-devices-toggle"
        >
          {p.devicesOpen ? <ChevronDown size={14} /> : <ChevronRight size={14} className="rtl:rotate-180" />}
          <Users size={14} className="text-[var(--color-muted)]" aria-hidden />
          <span className="flex-1">{tr("cs_trust_title", undefined, "Devices allowed to use it")}</span>
          {trustedCount !== null ? <Badge tone="neutral">{trustedCount}</Badge> : null}
        </button>
        {p.devicesOpen ? <div className="mt-2">{p.trusted}</div> : null}
      </div>
    </article>
  );
}

/** The live card for one roster profile. */
export function ConsoleCard({
  profile,
  appTailnet,
  appVersion,
  highlight,
  defaultOpen,
}: {
  profile: PS5Profile;
  appTailnet: AppTailnetStatus | null;
  appVersion: string | null;
  highlight?: boolean;
  defaultOpen?: boolean;
}) {
  const tr = useTr();
  const navigate = useNavigate();
  const host = profile.host.trim();
  const runtime = useHostRuntime(host);
  const activeId = useRosterStore((s) => s.active_id);
  const setActive = useRosterStore((s) => s.setActive);
  const rename = useRosterStore((s) => s.rename);
  const setNotes = useRosterStore((s) => s.setNotes);
  const remove = useRosterStore((s) => s.remove);
  const updateHost = useRosterStore((s) => s.updateHost);
  const stopped = useHelperStopped(host);
  const send = useHelperSendStore((st) => helperSendFor(st, host));
  const mesh = useMeshStore((s) => meshFor(s, host));
  const wake = useConsoleWake(host);
  const { guard, dialog: guardDialog } = useHelperReplaceGuard();
  const { confirm, dialog: confirmDialog } = useConfirm();
  const { prompt, dialog: promptDialog } = usePrompt();
  const [tst, setTst] = useState<TailnetStatus | null>(null);
  const [tsBusy, setTsBusy] = useState(false);
  const [tsOpen, setTsOpen] = useState(false);
  const [devicesOpen, setDevicesOpen] = useState(!!defaultOpen);
  const [inviteOpen, setInviteOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [answerBusy, setAnswerBusy] = useState(false);
  /** Bumped by Test: the Tailscale panel runs the test (one place for it and its end). */
  const [testNonce, setTestNonce] = useState(0);

  const status = cardStatus({
    host,
    runtime,
    tailnet: consoleOnTailnet(appTailnet, host),
    appVersion,
    stopped,
    sending: send?.state === "busy",
    lastSeenKernel: profile.last_seen_kernel,
  });
  const up = runtime.payloadStatus === "up" && runtime.session !== "needs_pairing";

  // The console's own Tailscale and its device list, while its helper answers.
  useEffect(() => {
    if (!up) return;
    let live = true;
    const read = async () => {
      try {
        const st = await tailnetStatus(host);
        if (live) setTst(st);
      } catch (e) {
        if (live && e instanceof TailnetUnsupportedError) setTst(null);
      }
    };
    // The selected console's list is read by useMeshSync already; nothing is read while data moves
    // to or from this console (the helper serves the transfer first).
    const selected = () => useRosterStore.getState().profiles.find((x) => x.id === useRosterStore.getState().active_id);
    const mine = () => !!selected() && hostOf(selected()!.host) === hostOf(host);
    void read();
    if (!mine()) void pollMesh(host);
    const id = window.setInterval(() => {
      if (document.hidden || consoleBusy(host) || mine()) return;
      void pollMesh(host);
    }, 6000);
    const tid = window.setInterval(() => {
      if (!document.hidden && !consoleBusy(host)) void read();
    }, 15000);
    return () => {
      live = false;
      window.clearInterval(id);
      window.clearInterval(tid);
    };
  }, [up, host]);

  const tn = tailnetSummary(up ? tst : null, appTailnet, host);

  const resend = async () => {
    if (!(await guard(host))) return;
    const conn = useConnectionStore.getState();
    const active = hostOf(conn.host) === hostOf(host);
    const replacing = active ? conn.payloadVersion : runtime.payloadVersion;
    if (active) conn.setStatus({ payloadVersion: null, ps5Kernel: null });
    await sendHelperTo(host, tr, replacing);
  };

  /** A change to the console's own Tailscale: its answer is the console's status. */
  const ts = async (f: () => Promise<TailnetStatus>) => {
    setTsBusy(true);
    setError(null);
    try {
      setTst(await f());
    } catch (e) {
      setError(tr("tn_failed", { err: humanizeCaught(e) }, "Could not change Tailscale on this PS5: {err}"));
    } finally {
      setTsBusy(false);
    }
  };

  return (
    <>
      {guardDialog}
      {confirmDialog}
      {promptDialog}
      <ConsoleCardView
        profile={profile}
        active={profile.id === activeId}
        status={status}
        tn={tn}
        mesh={up ? mesh.state : null}
        canWake={wake.canWake}
        wakeBusy={wake.busy}
        wakeError={wake.error}
        tsBusy={tsBusy}
        tsOpen={tsOpen}
        devicesOpen={devicesOpen}
        highlight={highlight}
        error={error}
        checklist={<SetupChecklist host={host} status={status} active={profile.id === activeId} />}
        priorKilled={up && runtime.priorInstance === "killed_externally"}
        tailnetPanel={<TailnetPanel host={host} startTest={testNonce} />}
        trusted={<TrustedDevices host={host} consoleName={profile.name} state={up ? mesh.state : null} />}
        stop={<StopHelperButton host={host} size="sm" />}
        onUse={() => setActive(profile.id)}
        onResend={() => void resend()}
        onWake={() => void wake.wake()}
        onSettings={() => navigate(ps5SettingsLink(profile.id))}
        onInvite={() => setInviteOpen(true)}
        onRename={async () => {
          const name = await prompt({
            title: tr("cs_card_rename_title", undefined, "Rename this PS5"),
            defaultValue: profile.name,
            okLabel: tr("save", undefined, "Save"),
          });
          if (name !== null && name.trim()) rename(profile.id, name);
        }}
        onNotes={async () => {
          const notes = await prompt({
            title: tr("cs_card_notes_title", undefined, "Notes for this PS5"),
            message: tr(
              "roster_notes_placeholder",
              undefined,
              "Notes (e.g. firmware quirks, do-not-update warnings)",
            ),
            defaultValue: profile.notes ?? "",
            okLabel: tr("save", undefined, "Save"),
          });
          if (notes !== null) setNotes(profile.id, notes);
        }}
        onRemove={async () => {
          const ok = await confirm({
            title: tr("roster_remove_title", { name: profile.name }, `Remove "${profile.name}"?`),
            message: tr(
              "roster_remove_body",
              undefined,
              "Profile and notes are deleted from this app. The PS5 itself isn't touched.",
            ),
            destructive: true,
            confirmLabel: tr("remove", undefined, "Remove"),
          });
          if (ok) remove(profile.id);
        }}
        onSaveHome={(v) => updateHost(profile.id, v)}
        onTsSwitch={(on) => void ts(() => tailnetOp(host, on ? "enable" : "disable"))}
        onTsTest={() => {
          // The panel runs it (its own answer is this device's Tailscale, never the console's).
          setTsOpen(true);
          setTestNonce((n) => n + 1);
        }}
        onToggleTs={() => {
          if (tsOpen) setTestNonce(0); // reopening the panel later does not test again
          setTsOpen((v) => !v);
        }}
        onToggleDevices={() => setDevicesOpen((v) => !v)}
        answerBusy={answerBusy}
        onAnswer={(op, id) => {
          if (answerBusy) return;
          setAnswerBusy(true);
          setError(null);
          void meshDevice(host, op, id)
            .then(() => {
              resetMeshCursor(host);
              return pollMesh(host);
            })
            .catch((e) => setError(humanizeCaught(e)))
            .finally(() => setAnswerBusy(false));
        }}
      />
      <InviteSheet
        open={inviteOpen}
        onClose={() => setInviteOpen(false)}
        name={profile.name}
        host={host}
        tailnetIp={tn.ip}
        state={up ? mesh.state : null}
      />
    </>
  );
}
