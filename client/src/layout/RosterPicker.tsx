import { useEffect, useId, useRef, useState } from "react";
import { useNavigate } from "react-router";
import {
  Cable,
  ChevronDown,
  ChevronRight,
  Plus,
  Check,
  ChevronUp,
  ChevronDown as ChevronDownIcon,
  ListOrdered,
} from "lucide-react";
import { useRosterStore, useActiveProfile, type PS5Profile } from "../state/roster";
import { useTr, type Translator } from "../state/lang";
import { Modal } from "../components/Modal";
import { parsePS5Firmware } from "../lib/ps5Firmware";
import { menuKeyTarget } from "../lib/menuKeys";
import { useFitOnScreen } from "../components/useFitOnScreen";
import { useBackDismiss } from "../lib/systemBack";
import { consoleOnTailnet, useTailnet, type ConsoleOnTailnet } from "../state/useTailnet";
import { useHostRuntime, type HostRuntime } from "../state/connection";
import { hostOf } from "../lib/addr";

/** How one console is reached right now, for the picker: a status dot and the path in words. */
export function pickerPath(
  tr: Translator,
  host: string,
  rt: HostRuntime,
  tailnet: ConsoleOnTailnet | null,
): { dot: "good" | "warn" | "rest" | "off"; text: string } {
  const answering = rt.payloadStatus === "up" || rt.session === "needs_pairing" || rt.power === "awake";
  if (rt.power === "rest") return { dot: "rest", text: tr("cs_card_rest", undefined, "In rest mode") };
  if (!answering) {
    return rt.payloadStatus === "down" || rt.power === "unreachable"
      ? { dot: "off", text: tr("cs_card_offline", undefined, "Not reachable") }
      : { dot: "off", text: hostOf(host) };
  }
  const dot = rt.payloadStatus === "up" && rt.session !== "needs_pairing" ? "good" : "warn";
  if (tailnet && tailnet.via !== "lan")
    return { dot, text: tr("cs_pick_via_tailscale", { ip: tailnet.ip }, "Tailscale {ip}") };
  return { dot, text: tr("cs_pick_via_home", { ip: hostOf(host) }, "Home {ip}") };
}

const DOT: Record<"good" | "warn" | "rest" | "off", string> = {
  good: "bg-[var(--color-good)]",
  warn: "bg-[var(--color-warn)]",
  rest: "bg-[var(--color-accent-bright)]",
  off: "bg-[var(--color-muted)] opacity-60",
};

function StatusDot({ dot }: { dot: keyof typeof DOT }) {
  return <span className={`inline-block h-2 w-2 shrink-0 rounded-full ${DOT[dot]}`} aria-hidden />;
}

/** One console's path line (its own runtime, read per row). */
function PathLine({ p, tailnet }: { p: PS5Profile; tailnet: ReturnType<typeof useTailnet>["status"] }) {
  const tr = useTr();
  const rt = useHostRuntime(p.host);
  const path = pickerPath(tr, p.host, rt, consoleOnTailnet(tailnet, p.host));
  return (
    <div className="flex min-w-0 items-center gap-1.5 text-xs text-[var(--color-muted)]" data-testid="roster-path">
      <StatusDot dot={path.dot} />
      <span className="truncate">{path.text}</span>
    </div>
  );
}

/**
 * Sidebar dropdown for switching between known PS5s.
 *
 * Each row shows the console's status dot and how it is reached right now (home or Tailscale).
 * Adding a console, and changing one, happen on the Consoles page (Connection): "Add a PS5"
 * opens its Add sheet, and "Manage PS5s" only orders the list and opens a console's card.
 */
export default function RosterPicker() {
  const tr = useTr();
  const navigate = useNavigate();
  const profiles = useRosterStore((s) => s.profiles);
  const setActive = useRosterStore((s) => s.setActive);
  const active = useActiveProfile();
  const [open, setOpen] = useState(false);
  const { status: tailnet } = useTailnet();
  const [manageOpen, setManageOpen] = useState(false);
  const wrapperRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const menuFit = useFitOnScreen(menuRef, open);
  const menuId = useId();

  const menuItems = () =>
    Array.from(menuRef.current?.querySelectorAll<HTMLElement>('[role^="menuitem"]') ?? []);

  // Opening moves focus into the list (the console on show, else the first row), so the arrow
  // keys work straight away.
  useEffect(() => {
    if (!open) return;
    const items = menuItems();
    (items.find((el) => el.getAttribute("aria-checked") === "true") ?? items[0])?.focus();
  }, [open]);

  /** Close and hand focus back to the trigger (Escape, Tab, picking a row). */
  const closeMenu = () => {
    setOpen(false);
    triggerRef.current?.focus();
  };
  useBackDismiss(open, closeMenu);

  const onMenuKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Escape" || e.key === "Tab") {
      e.preventDefault();
      e.stopPropagation();
      closeMenu();
      return;
    }
    const items = menuItems();
    const next = menuKeyTarget(e.key, items.indexOf(document.activeElement as HTMLElement), items.length);
    if (next === null) return;
    e.preventDefault();
    items[next]?.focus();
  };

  useEffect(() => {
    if (!open) return;
    function onClick(e: MouseEvent) {
      if (wrapperRef.current && !wrapperRef.current.contains(e.target as Node)) setOpen(false);
    }
    document.addEventListener("mousedown", onClick, true);
    return () => document.removeEventListener("mousedown", onClick, true);
  }, [open]);

  const fwLabel = active?.last_seen_kernel ? (parsePS5Firmware(active.last_seen_kernel) ?? "—") : null;

  return (
    <div className="relative mx-4 mb-1" ref={wrapperRef}>
      <button
        ref={triggerRef}
        id={`${menuId}-trigger`}
        type="button"
        onClick={() => setOpen((v) => !v)}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown" && !open) {
            e.preventDefault();
            setOpen(true);
          }
        }}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? menuId : undefined}
        className="flex min-h-12 w-full items-center gap-2.5 rounded-full border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] py-1.5 ps-1.5 pe-4 text-start text-xs shadow-[var(--edge-highlight),var(--shadow-1)] transition-colors hover:bg-[var(--color-float)]"
      >
        <span className="grid h-9 w-9 shrink-0 place-items-center rounded-full bg-[var(--color-accent-soft)] text-[var(--color-accent-bright)]">
          <Cable size={15} />
        </span>
        <div className="min-w-0 flex-1 leading-tight">
          {active ? (
            <>
              <div className="truncate font-medium text-[var(--color-text)]">
                {active.name}
                {fwLabel && <span className="font-normal text-[var(--color-muted)]">{` · FW ${fwLabel}`}</span>}
              </div>
              <PathLine p={active} tailnet={tailnet} />
            </>
          ) : (
            <span className="text-[var(--color-muted)]">
              {tr("roster_no_ps5", undefined, "No PS5 added — click to add")}
            </span>
          )}
        </div>
        <ChevronDown
          size={12}
          className={`shrink-0 text-[var(--color-muted)] transition-transform ${open ? "rotate-180" : ""}`}
        />
      </button>

      {open && (
        <div
          ref={menuRef}
          id={menuId}
          role="menu"
          aria-labelledby={`${menuId}-trigger`}
          onKeyDown={onMenuKeyDown}
          className="glass-float elev-2 absolute left-0 right-0 top-[calc(100%+0.35rem)] z-40 overflow-y-auto rounded-[var(--radius-card)] py-1.5"
          style={menuFit}
        >
          {profiles.length > 1 && (
            <ul className="max-h-48 overflow-y-auto" role="none">
              {profiles.map((p) => (
                <li key={p.id} role="none">
                  <button
                    type="button"
                    role="menuitemradio"
                    aria-checked={p.id === active?.id}
                    onClick={() => {
                      setActive(p.id);
                      closeMenu();
                    }}
                    className={`flex w-full items-center gap-2 px-4 py-2 text-start text-xs hover:bg-[var(--color-surface-3)] ${
                      p.id === active?.id ? "bg-[var(--color-surface-3)]" : ""
                    }`}
                  >
                    {p.id === active?.id ? (
                      <Check size={11} className="shrink-0 text-[var(--color-good)]" />
                    ) : (
                      <span className="w-[11px] shrink-0" />
                    )}
                    <div className="min-w-0 flex-1">
                      <div className="truncate">{p.name}</div>
                      <PathLine p={p} tailnet={tailnet} />
                    </div>
                  </button>
                </li>
              ))}
            </ul>
          )}
          <button
            type="button"
            role="menuitem"
            onClick={() => {
              closeMenu();
              navigate("/connection?add=1");
            }}
            className="flex w-full items-center gap-2 border-t border-[var(--color-border)] px-4 py-2 text-start text-xs hover:bg-[var(--color-surface-3)]"
            data-testid="roster-add"
          >
            <Plus size={11} />
            {tr("cs_add_title", undefined, "Add a PS5")}
          </button>
          {profiles.length > 0 && (
            <button
              type="button"
              role="menuitem"
              onClick={() => {
                // Focus goes back to the trigger first, so the dialog has somewhere to return
                // it to when it closes (this row is gone).
                closeMenu();
                setManageOpen(true);
              }}
              className="flex w-full items-center gap-2 px-4 py-2 text-start text-xs hover:bg-[var(--color-surface-3)]"
              data-testid="roster-manage"
            >
              <ListOrdered size={11} />
              {tr("roster_manage", undefined, "Manage PS5s")}
            </button>
          )}
        </div>
      )}

      {manageOpen && <RosterManageModal tailnet={tailnet} onClose={() => setManageOpen(false)} />}
    </div>
  );
}

/** Manage PS5s: the order of the list (the console tabs and this picker follow it), and a way to
 *  each console's card, where it is renamed, re-addressed or removed. */
export function RosterManageModal({
  onClose,
  tailnet,
}: {
  onClose: () => void;
  tailnet: ReturnType<typeof useTailnet>["status"];
}) {
  const tr = useTr();
  const navigate = useNavigate();
  const profiles = useRosterStore((s) => s.profiles);
  const reorder = useRosterStore((s) => s.reorder);
  const active = useActiveProfile();

  return (
    <Modal open onClose={onClose} title={tr("roster_manage_title", undefined, "Manage PS5s")} size="md">
      <div className="p-4 text-sm" data-testid="roster-manage-modal">
        {profiles.length > 1 && (
          <p className="mb-2 text-xs text-[var(--color-muted)]">
            {tr("roster_order_hint", undefined, "This order sets the console tabs and the picker.")}
          </p>
        )}
        <ul className="space-y-2">
          {profiles.map((p, index) => (
            <li
              key={p.id}
              className={`flex items-center gap-2 rounded-[var(--radius-field)] border p-2.5 text-xs ${
                p.id === active?.id ? "border-[var(--color-accent)]" : "border-[var(--color-border)]"
              }`}
            >
              <div className="flex shrink-0 flex-col">
                <button
                  type="button"
                  disabled={index === 0}
                  onClick={() => reorder(index, index - 1)}
                  aria-label={tr("roster_move_up_aria", { name: p.name }, `Move ${p.name} up`)}
                  title={tr("roster_move_up", undefined, "Move up")}
                  className="rounded-full p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface)] hover:text-[var(--color-text)] disabled:cursor-not-allowed disabled:opacity-30"
                >
                  <ChevronUp size={13} />
                </button>
                <button
                  type="button"
                  disabled={index === profiles.length - 1}
                  onClick={() => reorder(index, index + 1)}
                  aria-label={tr("roster_move_down_aria", { name: p.name }, `Move ${p.name} down`)}
                  title={tr("roster_move_down", undefined, "Move down")}
                  className="rounded-full p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface)] hover:text-[var(--color-text)] disabled:cursor-not-allowed disabled:opacity-30"
                >
                  <ChevronDownIcon size={13} />
                </button>
              </div>
              <div className="min-w-0 flex-1">
                <div className="truncate font-medium">{p.name}</div>
                <PathLine p={p} tailnet={tailnet} />
              </div>
              <button
                type="button"
                onClick={() => {
                  onClose();
                  navigate(`/connection?console=${encodeURIComponent(p.id)}`);
                }}
                className="inline-flex shrink-0 items-center gap-1 rounded-full px-2.5 py-1 text-xs font-medium text-[var(--color-accent-bright)] hover:bg-[var(--color-accent-soft)]"
                data-testid="roster-open-card"
              >
                {tr("cs_pick_open", undefined, "Open")}
                <ChevronRight size={12} className="rtl:rotate-180" />
              </button>
            </li>
          ))}
        </ul>
        <button
          type="button"
          onClick={() => {
            onClose();
            navigate("/connection?add=1");
          }}
          className="mt-3 inline-flex items-center gap-1.5 rounded-full bg-[image:var(--accent-fill)] px-3 py-1.5 text-xs font-semibold text-[var(--color-accent-contrast)] shadow-[var(--accent-glow)] hover:brightness-105"
        >
          <Plus size={12} />
          {tr("cs_add_title", undefined, "Add a PS5")}
        </button>
      </div>
    </Modal>
  );
}
