import { useEffect, useRef, useState } from "react";
import {
  Cable,
  ChevronDown,
  Plus,
  Trash2,
  Pencil,
  Check,
  ChevronUp,
  ChevronDown as ChevronDownIcon,
} from "lucide-react";
import {
  useRosterStore,
  useActiveProfile,
  type PS5Profile,
} from "../state/roster";
import { useTr } from "../state/lang";
// Direct import (not via components/index.ts barrel) — the barrel
// re-exports from this module's parent surface, which Rollup flags
// as a circular dep warning at build time. Direct path keeps the
// chunk graph clean.
import { useConfirm } from "../components/ConfirmDialog";
import { Modal } from "../components/Modal";
import { parsePS5Firmware } from "../lib/ps5Firmware";

/**
 * Sidebar dropdown for switching between known PS5s.
 *
 * Always renders; when no profiles exist it shows an "Add PS5"
 * affordance directly. When one profile exists it acts as a
 * compact identity badge (with the dropdown still available for
 * Manage). When multiple profiles exist it surfaces the picker.
 *
 * Mounting strategy: lives in the sidebar between the brand header
 * and the nav, so it's always visible regardless of which screen
 * the user is on. Switching active profile invalidates the
 * connection store's host, which kicks off AppShell's status poll
 * for the new PS5 — no manual refresh needed downstream.
 *
 * Click-outside dismissal: a single document listener wired via
 * useEffect. We capture-phase to avoid race conditions with the
 * inner button's onClick (which would otherwise re-open right
 * after the document handler closed it).
 */
export default function RosterPicker() {
  const tr = useTr();
  const profiles = useRosterStore((s) => s.profiles);
  const setActive = useRosterStore((s) => s.setActive);
  const active = useActiveProfile();
  const [open, setOpen] = useState(false);
  const [manageOpen, setManageOpen] = useState(false);
  const wrapperRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    function onClick(e: MouseEvent) {
      if (
        wrapperRef.current &&
        !wrapperRef.current.contains(e.target as Node)
      ) {
        setOpen(false);
      }
    }
    document.addEventListener("mousedown", onClick, true);
    return () => document.removeEventListener("mousedown", onClick, true);
  }, [open]);

  const fwLabel = active?.last_seen_kernel
    ? (parsePS5Firmware(active.last_seen_kernel) ?? "—")
    : null;

  return (
    <div className="relative mx-2 mb-1" ref={wrapperRef}>
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        // min-h-11 (44px) meets the touch-target floor (mobile-design
        // §4.1). It's only +3px over the natural height, so desktop is
        // visually unchanged — no need to make it conditional.
        className="flex min-h-12 w-full items-center gap-2.5 rounded-[0.7rem] border border-[var(--color-border)] bg-[color-mix(in_oklab,var(--color-surface)_45%,transparent)] px-3 py-2 text-left text-xs shadow-sm hover:border-[var(--color-border-strong)] hover:bg-[var(--color-surface-3)]"
      >
        <span className="grid h-7 w-7 shrink-0 place-items-center rounded-lg bg-[var(--color-surface-3)] text-[var(--color-muted)]">
          <Cable size={13} />
        </span>
        <div className="min-w-0 flex-1 leading-tight">
          {active ? (
            <>
              <div className="truncate font-medium text-[var(--color-text)]">
                {active.name}
              </div>
              <div className="truncate text-xs text-[var(--color-muted)]">
                {active.host}
                {fwLabel && ` · FW ${fwLabel}`}
              </div>
            </>
          ) : (
            <span className="text-[var(--color-muted)]">
              {tr("roster_no_ps5", undefined, "No PS5 added — click to add")}
            </span>
          )}
        </div>
        <ChevronDown
          size={12}
          className={`shrink-0 text-[var(--color-muted)] transition-transform ${
            open ? "rotate-180" : ""
          }`}
        />
      </button>

      {open && (
        <div className="elev-2 absolute left-0 right-0 top-[calc(100%+0.35rem)] z-40 overflow-hidden rounded-[0.7rem] border border-[var(--color-border)] bg-[var(--color-surface-raised)] py-1">
          {profiles.length > 1 && (
            <ul className="max-h-40 overflow-y-auto">
              {profiles.map((p) => (
                <li key={p.id}>
                  <button
                    type="button"
                    onClick={() => {
                      setActive(p.id);
                      setOpen(false);
                    }}
                    className={`flex w-full items-center gap-2 px-4 py-1.5 text-left text-xs hover:bg-[var(--color-surface-2)] ${
                      p.id === active?.id ? "bg-[var(--color-surface-2)]" : ""
                    }`}
                  >
                    {p.id === active?.id ? (
                      <Check
                        size={11}
                        className="shrink-0 text-[var(--color-good)]"
                      />
                    ) : (
                      <span className="w-[11px] shrink-0" />
                    )}
                    <div className="min-w-0 flex-1">
                      <div className="truncate">{p.name}</div>
                      <div className="truncate text-xs text-[var(--color-muted)]">
                        {p.host}
                      </div>
                    </div>
                  </button>
                </li>
              ))}
            </ul>
          )}
          <button
            type="button"
            onClick={() => {
              setOpen(false);
              setManageOpen(true);
            }}
            className="flex w-full items-center gap-2 border-t border-[var(--color-border)] px-4 py-1.5 text-left text-xs hover:bg-[var(--color-surface-2)]"
          >
            <Pencil size={11} />
            {tr("roster_manage", undefined, "Manage PS5s")}
          </button>
        </div>
      )}

      {manageOpen && <RosterManageModal onClose={() => setManageOpen(false)} />}
    </div>
  );
}

function RosterManageModal({ onClose }: { onClose: () => void }) {
  const tr = useTr();
  const profiles = useRosterStore((s) => s.profiles);
  const add = useRosterStore((s) => s.add);
  const remove = useRosterStore((s) => s.remove);
  const rename = useRosterStore((s) => s.rename);
  const reorder = useRosterStore((s) => s.reorder);
  const updateHost = useRosterStore((s) => s.updateHost);
  const setNotes = useRosterStore((s) => s.setNotes);
  const active = useActiveProfile();
  const { confirm: confirmDialog, dialog: confirmDialogNode } = useConfirm();

  const [newName, setNewName] = useState("");
  const [newHost, setNewHost] = useState("");
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editName, setEditName] = useState("");
  const [editHost, setEditHost] = useState("");
  const [editNotes, setEditNotes] = useState("");

  function handleAdd() {
    if (!newHost.trim()) return;
    add({ name: newName, host: newHost });
    setNewName("");
    setNewHost("");
  }

  function startEdit(p: PS5Profile) {
    setEditingId(p.id);
    setEditName(p.name);
    setEditHost(p.host);
    setEditNotes(p.notes ?? "");
  }

  function commitEdit() {
    if (!editingId) return;
    rename(editingId, editName);
    updateHost(editingId, editHost);
    setNotes(editingId, editNotes);
    setEditingId(null);
  }

  return (
    <>
      {/* Nested delete-confirmation renders its own top-level scrim, so it
          sits outside the Modal panel and isn't dismissed by it. */}
      {confirmDialogNode}
      <Modal
        open
        onClose={onClose}
        title={tr("roster_manage_title", undefined, "Manage PS5s")}
        size="lg"
      >
        <div className="p-4 text-sm">
          {profiles.length === 0 && (
            <p className="mb-3 text-xs text-[var(--color-muted)]">
              {tr(
                "roster_empty",
                undefined,
                "No PS5s yet. Add one below to get started.",
              )}
            </p>
          )}
          {profiles.length > 1 && (
            <p className="mb-2 text-[0.7rem] text-[var(--color-muted)]">
              {tr(
                "roster_order_hint",
                undefined,
                "This order sets the console tabs and the picker.",
              )}
            </p>
          )}
          <ul className="space-y-2">
            {profiles.map((p, index) => (
              <li
                key={p.id}
                className={`rounded-md border p-3 text-xs ${
                  p.id === active?.id
                    ? "border-[var(--color-accent)]"
                    : "border-[var(--color-border)]"
                }`}
              >
                {editingId === p.id ? (
                  <div className="space-y-2">
                    <div className="flex flex-wrap items-center gap-2">
                      <input
                        value={editName}
                        onChange={(e) => setEditName(e.target.value)}
                        placeholder={tr("roster_edit_name_placeholder", "Name")}
                        className="flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1"
                      />
                      <input
                        value={editHost}
                        onChange={(e) => setEditHost(e.target.value)}
                        placeholder="192.168.1.50"
                        inputMode="decimal"
                        className="flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1"
                      />
                    </div>
                    <textarea
                      value={editNotes}
                      onChange={(e) => setEditNotes(e.target.value)}
                      rows={3}
                      maxLength={2000}
                      placeholder={tr(
                        "roster_notes_placeholder",
                        undefined,
                        "Notes (e.g. firmware quirks, do-not-update warnings)",
                      )}
                      className="w-full resize-y rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 text-xs"
                    />
                    <div className="flex items-center gap-2">
                      <button
                        type="button"
                        onClick={commitEdit}
                        className="rounded-md bg-[var(--color-accent)] px-2 py-1 text-[var(--color-accent-contrast)]"
                      >
                        {tr("save", undefined, "Save")}
                      </button>
                      <button
                        type="button"
                        onClick={() => setEditingId(null)}
                        className="rounded-md px-2 py-1 text-[var(--color-muted)]"
                      >
                        {tr("cancel", undefined, "Cancel")}
                      </button>
                    </div>
                  </div>
                ) : (
                  <div className="flex items-center gap-2">
                    <div className="min-w-0 flex-1">
                      <div className="font-medium">{p.name}</div>
                      <div className="text-[var(--color-muted)]">{p.host}</div>
                      {p.last_seen_at && (
                        <div className="mt-0.5 text-xs text-[var(--color-muted)]">
                          {tr(
                            "roster_last_seen",
                            {
                              when: new Date(
                                p.last_seen_at * 1000,
                              ).toLocaleString(),
                            },
                            `last seen: ${new Date(p.last_seen_at * 1000).toLocaleString()}`,
                          )}
                          {p.last_seen_payload &&
                            ` · ps5upload v${p.last_seen_payload}`}
                        </div>
                      )}
                      {p.notes && (
                        <div className="mt-1 whitespace-pre-wrap rounded border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 text-xs italic text-[var(--color-muted)]">
                          {p.notes}
                        </div>
                      )}
                    </div>
                    <button
                      type="button"
                      onClick={() => startEdit(p)}
                      aria-label={tr("roster_edit", undefined, "Edit PS5")}
                      title={tr("roster_edit", undefined, "Edit PS5")}
                      className="rounded-md p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]"
                    >
                      <Pencil size={12} />
                    </button>
                    <button
                      type="button"
                      disabled={index === 0}
                      onClick={() => reorder(index, index - 1)}
                      aria-label={tr(
                        "roster_move_up_aria",
                        { name: p.name },
                        `Move ${p.name} up`,
                      )}
                      title={tr("roster_move_up", undefined, "Move up")}
                      className="rounded-md p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface)] hover:text-[var(--color-text)] disabled:cursor-not-allowed disabled:opacity-30"
                    >
                      <ChevronUp size={12} />
                    </button>
                    <button
                      type="button"
                      disabled={index === profiles.length - 1}
                      onClick={() => reorder(index, index + 1)}
                      aria-label={tr(
                        "roster_move_down_aria",
                        { name: p.name },
                        `Move ${p.name} down`,
                      )}
                      title={tr("roster_move_down", undefined, "Move down")}
                      className="rounded-md p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface)] hover:text-[var(--color-text)] disabled:cursor-not-allowed disabled:opacity-30"
                    >
                      <ChevronDownIcon size={12} />
                    </button>
                    <button
                      type="button"
                      onClick={async () => {
                        const ok = await confirmDialog({
                          title: tr(
                            "roster_remove_title",
                            { name: p.name },
                            `Remove "${p.name}"?`,
                          ),
                          message: tr(
                            "roster_remove_body",
                            undefined,
                            "Profile and notes are deleted from this app. The PS5 itself isn't touched.",
                          ),
                          destructive: true,
                          confirmLabel: tr("remove", undefined, "Remove"),
                        });
                        if (ok) remove(p.id);
                      }}
                      aria-label={tr(
                        "roster_remove_aria",
                        { name: p.name },
                        `Remove ${p.name}`,
                      )}
                      title={tr("remove", undefined, "Remove")}
                      className="rounded-md p-1 text-[var(--color-muted)] hover:bg-[var(--color-bad-soft)] hover:text-[var(--color-bad)]"
                    >
                      <Trash2 size={12} />
                    </button>
                  </div>
                )}
              </li>
            ))}
          </ul>

          <div className="mt-4 border-t border-[var(--color-border)] pt-4">
            <h3 className="mb-2 text-xs font-semibold">
              {tr("roster_add_title", undefined, "Add a PS5")}
            </h3>
            <div className="flex flex-wrap items-center gap-2">
              <input
                value={newName}
                onChange={(e) => setNewName(e.target.value)}
                placeholder={tr(
                  "roster_name_placeholder",
                  undefined,
                  "Living-room PS5",
                )}
                className="flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 text-xs"
              />
              <input
                value={newHost}
                onChange={(e) => setNewHost(e.target.value)}
                placeholder="192.168.1.50"
                inputMode="decimal"
                className="flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-2 py-1 text-xs"
              />
              <button
                type="button"
                onClick={handleAdd}
                disabled={!newHost.trim()}
                className="inline-flex items-center gap-1 rounded-md bg-[var(--color-accent)] px-2 py-1 text-xs text-[var(--color-accent-contrast)] disabled:opacity-50"
              >
                <Plus size={11} />
                {tr("add", undefined, "Add")}
              </button>
            </div>
          </div>
        </div>
      </Modal>
    </>
  );
}
