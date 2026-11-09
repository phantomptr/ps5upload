// The pieces both sides of Share are built from: a row to pick (or not) one thing, a quiet info
// tip, the "Your name" line, and a section label. Only the app's own components and tokens.

import { useEffect, useState, type ReactNode } from "react";
import {
  AppWindow,
  Check,
  FileText,
  Film,
  Gamepad2,
  Info,
  MonitorPlay,
  Pencil,
  Save,
  Sparkles,
  type LucideIcon,
} from "lucide-react";

import type { ShareKind } from "../../api/share";
import { loadMeshMe } from "../../state/mesh";
import { useMeshStore } from "../../state/mesh";
import { useTr } from "../../state/lang";
import { savedShareName } from "./shareModel";

/** Each kind's icon. */
export const KIND_ICON: Record<ShareKind, LucideIcon> = {
  app: AppWindow,
  save: Save,
  media: Film,
  cheats: Sparkles,
  controls: Gamepad2,
  stream: MonitorPlay,
  note: FileText,
};

/** A small heading over a group of the panel. */
export function ShareLabel({ children, id }: { children: ReactNode; id?: string }) {
  return (
    <h3 id={id} className="text-xs font-semibold uppercase tracking-[0.06em] text-[var(--color-muted)]">
      {children}
    </h3>
  );
}

/** An info icon whose text shows on hover or focus (a tap on a phone). */
export function InfoTip({ text, testId }: { text: string; testId?: string }) {
  return (
    <span className="group relative inline-flex shrink-0">
      <button
        type="button"
        aria-label={text}
        title={text}
        data-testid={testId}
        className="grid h-7 w-7 place-items-center rounded-full text-[var(--color-muted)] hover:text-[var(--color-text)] focus-visible:outline-2 focus-visible:outline-[var(--color-accent)] max-md:h-10 max-md:w-10"
        onClick={(e) => e.stopPropagation()}
      >
        <Info size={15} aria-hidden />
      </button>
      <span
        role="tooltip"
        className="pointer-events-none absolute right-0 top-full z-10 mt-1 hidden w-64 max-w-[calc(100vw-4rem)] rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-float)] px-3 py-2 text-xs font-normal leading-snug text-[var(--color-text)] shadow-[var(--shadow-2)] group-focus-within:block group-hover:block"
      >
        {text}
      </span>
    </span>
  );
}

/**
 * One thing to share (or to accept): its icon, name, a short line, its size, and a check. A
 * row that can't be picked is quiet: muted, no check, its reason on the line and in the tip.
 */
export function ItemRow({
  kind,
  name,
  line,
  size,
  checked,
  onChange,
  unavailable,
  reason,
  busy,
  testId,
  children,
}: {
  kind: ShareKind;
  name: string;
  line?: ReactNode;
  size?: string | null;
  checked: boolean;
  onChange?: (on: boolean) => void;
  /** Can't be picked: the one-line reason shows instead of `line`. */
  unavailable?: string | null;
  /** The longer reason, in the tip. */
  reason?: string | null;
  /** Still being checked. */
  busy?: boolean;
  testId?: string;
  /** Below the row while it is picked (the save's slots, the app's source). */
  children?: ReactNode;
}) {
  const Icon = KIND_ICON[kind];
  const off = !!unavailable || busy;
  const on = checked && !off;
  return (
    <div
      data-testid={testId}
      data-state={unavailable ? "unavailable" : busy ? "checking" : on ? "on" : "off"}
      className={[
        "rounded-[var(--radius-card)] border transition-colors",
        on
          ? "border-[var(--color-accent)] bg-[var(--color-accent-soft)]"
          : "border-[var(--color-border)] bg-[var(--color-surface-raised)]",
        unavailable ? "bg-transparent" : "",
      ].join(" ")}
    >
      <div className="flex items-center gap-1 pe-2">
        <button
          type="button"
          role="checkbox"
          aria-checked={on}
          aria-disabled={off || undefined}
          disabled={off}
          onClick={() => onChange?.(!checked)}
          className={[
            "flex min-h-14 min-w-0 flex-1 items-center gap-3 rounded-[var(--radius-card)] px-3 py-2.5 text-start outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-accent)]",
            off ? "cursor-default" : "cursor-pointer",
          ].join(" ")}
        >
          <span
            className={[
              "grid h-9 w-9 shrink-0 place-items-center rounded-[10px]",
              unavailable
                ? "bg-[var(--color-surface-2)] text-[var(--color-muted)]"
                : on
                  ? "bg-[var(--color-accent)] text-[var(--color-accent-contrast)]"
                  : "bg-[var(--color-surface-3)] text-[var(--color-text)]",
            ].join(" ")}
            aria-hidden
          >
            <Icon size={17} />
          </span>
          <span className="flex min-w-0 flex-1 flex-col">
            <span
              className={`truncate text-sm font-medium ${unavailable ? "text-[var(--color-muted)]" : "text-[var(--color-text)]"}`}
            >
              {name}
            </span>
            {unavailable ? (
              <span className="truncate text-xs text-[var(--color-muted)]" data-testid={testId ? `${testId}-reason` : undefined}>
                {unavailable}
              </span>
            ) : (
              (line || size) && (
                <span className="truncate text-xs text-[var(--color-muted)]">
                  {line}
                  {line && size ? " · " : ""}
                  {size && <span className="tabular-nums">{size}</span>}
                </span>
              )
            )}
          </span>
          {!unavailable && (
            <span
              aria-hidden
              className={[
                "grid h-6 w-6 shrink-0 place-items-center rounded-full border transition-colors",
                on
                  ? "border-transparent bg-[var(--color-accent)] text-[var(--color-accent-contrast)]"
                  : "border-[var(--color-border-strong)] text-transparent",
                busy ? "animate-pulse" : "",
              ].join(" ")}
            >
              <Check size={14} strokeWidth={3} />
            </span>
          )}
        </button>
        {unavailable && reason && <InfoTip text={reason} testId={testId ? `${testId}-why` : undefined} />}
      </div>
      {on && children && <div className="px-3 pb-3 ps-[3.75rem]">{children}</div>}
    </div>
  );
}

/** This device's name, as other devices see it (Devices), for the name a friend sees. */
export function useDeviceName(): string {
  const me = useMeshStore((s) => s.me);
  useEffect(() => {
    if (!me) void loadMeshMe();
  }, [me]);
  return me?.name?.trim() ?? "";
}

/**
 * "Your name": shown as text with an edit button, prefilled with what was typed last on this
 * device, else this device's name.
 */
export function NameField({
  value,
  onChange,
  testId,
}: {
  value: string;
  onChange: (v: string) => void;
  testId: string;
}) {
  const tr = useTr();
  const device = useDeviceName();
  const [editing, setEditing] = useState(false);
  // Prefill once the device's name is known, unless a name was kept or typed.
  useEffect(() => {
    if (!value && device && !savedShareName()) onChange(device.slice(0, 40));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [device]);
  const label = tr("share_your_name", undefined, "Your name");
  if (editing) {
    return (
      <div className="space-y-1">
        <label className="text-xs font-medium text-[var(--color-text)]" htmlFor={`${testId}-input`}>
          {label}
        </label>
        <input
          id={`${testId}-input`}
          className="input"
          value={value}
          maxLength={40}
          autoFocus
          onChange={(e) => onChange(e.target.value)}
          onBlur={() => setEditing(false)}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === "Escape") {
              e.preventDefault();
              setEditing(false);
            }
          }}
          data-testid={testId}
        />
      </div>
    );
  }
  return (
    <div className="flex min-h-11 items-center gap-3">
      <span className="min-w-0 flex-1">
        <span className="block text-xs text-[var(--color-muted)]">{label}</span>
        <span className="block truncate text-sm font-medium" data-testid={`${testId}-shown`}>
          {value.trim() || tr("share_name_none", undefined, "A friend")}
        </span>
      </span>
      <button
        type="button"
        onClick={() => setEditing(true)}
        className="chip min-h-8 shrink-0 gap-1.5 px-3 text-xs font-medium max-md:min-h-11"
        data-testid={`${testId}-edit`}
        aria-label={tr("share_name_edit_aria", undefined, "Change your name")}
      >
        <Pencil size={12} aria-hidden />
        {tr("share_name_edit", undefined, "Edit")}
      </button>
    </div>
  );
}

/** A note field: one line that grows as it fills. */
export function GrowingNote({
  value,
  onChange,
  max,
  placeholder,
  testId,
}: {
  value: string;
  onChange: (v: string) => void;
  max: number;
  placeholder: string;
  testId: string;
}) {
  return (
    <textarea
      rows={1}
      value={value}
      maxLength={max}
      placeholder={placeholder}
      aria-label={placeholder}
      onChange={(e) => {
        onChange(e.target.value);
        const t = e.currentTarget;
        t.style.height = "auto";
        t.style.height = `${Math.min(t.scrollHeight, 160)}px`;
      }}
      className="input block max-h-40 min-h-11 w-full resize-none overflow-y-auto leading-snug [field-sizing:content]"
      data-testid={testId}
    />
  );
}
