// "Name on the PS5" for a package picked on Upload & Install (#937): the name the package gives
// its game, in a field. Changing it rewrites the name inside the package file on the engine's
// computer (api/pkgName), before the package is installed. Only a fake package can be changed;
// an official one says why not.

import { useEffect, useMemo, useRef, useSyncExternalStore } from "react";
import { Type } from "lucide-react";

import { readPkgName, setPkgName } from "../../api/pkgName";
import { Button, Checkbox, Input } from "../../components";
import { useTr } from "../../state/lang";
import { createNameFlow, nameRoom, saveBlocked, type NameFlowState } from "./pkgNameFlow";

/** A line that appears after something was done (the name changed, or is too long) is brought
 *  into view, clear of the sticky actions and the tab bar (the scroll area's
 *  `scroll-padding-bottom`, index.css): on a phone it appeared under them. */
const showWhole = (el: HTMLElement | null) => el?.scrollIntoView?.({ block: "nearest" });

export function PackageNameView({
  state,
  disabled = false,
  onValue,
  onAllLanguages,
  onSave,
}: {
  state: NameFlowState;
  /** The package is being sent or checked: nothing here can be changed meanwhile. */
  disabled?: boolean;
  onValue: (value: string) => void;
  onAllLanguages: (all: boolean) => void;
  onSave: () => void;
}) {
  const tr = useTr();
  const { names } = state;
  // Nothing to show until the name is read, for a package that has none, and for a delta
  // update (its name is the game's).
  if (!names) return null;
  const refusal = names.refusal ?? null;
  if (refusal && (refusal.code === "patch" || refusal.code === "no_name")) return null;
  const title = tr("upload_pkg_name_title", undefined, "Name on the PS5");
  const locked = disabled || state.saving;
  // The package's room for the name, counted as the engine counts it (bytes): shown while the
  // name is being changed, and said here when it does not fit, before anything is sent.
  const room = nameRoom(state);
  const tooLong = room !== null && room.left < 0;
  const editing = room !== null && state.value.trim() !== names.name;
  return (
    <section
      className="rounded-[var(--radius-panel)] border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] shadow-[var(--edge-highlight),var(--shadow-1)] mb-3 grid gap-2 p-4"
      data-testid="pkg-name-card"
    >
      <div className="flex items-center gap-2 text-sm font-medium">
        <Type size={16} className="text-[var(--color-muted)]" aria-hidden />
        <span>{title}</span>
      </div>
      {refusal ? (
        <>
          {names.name && <div className="truncate text-sm">{names.name}</div>}
          <p className="text-xs text-[var(--color-muted)]" data-testid="pkg-name-refusal">
            {refusal.code === "retail"
              ? tr(
                  "upload_pkg_name_retail",
                  undefined,
                  "This is an official package, so its name can't be changed: the name is covered by its publisher's signature.",
                )
              : tr(
                  "upload_pkg_name_refused",
                  { reason: refusal.reason },
                  "This package's name can't be changed: {reason}",
                )}
          </p>
        </>
      ) : (
        <>
          <div className="flex flex-wrap items-start gap-2">
            <div className="min-w-0 flex-1 basis-[14rem]">
              <Input
                aria-label={title}
                // A game's name in its own direction: in Arabic a Latin name showed only its end.
                dir="auto"
                data-testid="pkg-name-input"
                value={state.value}
                // In characters, so only a first stop: the room is counted in bytes below.
                maxLength={names.max_bytes}
                aria-invalid={tooLong || undefined}
                aria-describedby={room && (tooLong || editing) ? "pkg-name-room" : undefined}
                disabled={locked}
                onChange={(e) => onValue(e.currentTarget.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") onSave();
                }}
              />
            </div>
            <Button
              variant="secondary"
              size="sm"
              data-testid="pkg-name-save"
              loading={state.saving}
              disabled={disabled || saveBlocked(state) !== null}
              onClick={onSave}
            >
              {tr("upload_pkg_name_save", undefined, "Change name")}
            </Button>
          </div>
          {room && tooLong ? (
            <p id="pkg-name-room" role="alert" ref={showWhole} className="text-xs text-[var(--color-bad)]" data-testid="pkg-name-too-long">
              {tr(
                "upload_pkg_name_too_long",
                { count: -room.left, limit: room.limit },
                "This name is too long for this package. Bytes that fit: {limit}. Bytes over: {count}. Make it shorter: letters with accents and non-Latin letters take more room.",
              )}
            </p>
          ) : (
            room &&
            editing && (
              <p id="pkg-name-room" className="text-xs tabular-nums text-[var(--color-muted)]" data-testid="pkg-name-room">
                {tr("upload_pkg_name_left", { count: room.left }, "Bytes left for the name: {count}")}
              </p>
            )
          )}
          {names.other_names > 0 && (
            <Checkbox
              checked={state.allLanguages}
              onChange={onAllLanguages}
              disabled={locked}
              label={tr(
                "upload_pkg_name_all_v2",
                { count: names.other_names },
                "Use this name in every language (other names in the package: {count})",
              )}
            />
          )}
          <p className="text-xs text-[var(--color-muted)]">
            {tr(
              "upload_pkg_name_hint",
              undefined,
              "Changes the name in the package file itself, before you install it. A game that is already installed keeps its name until you install it again.",
            )}
          </p>
          {state.saved && state.saved.changed > 0 && (
            <p ref={showWhole} className="text-xs text-[var(--color-good)]" data-testid="pkg-name-saved">
              {tr("upload_pkg_name_saved", undefined, "Name changed.")}
            </p>
          )}
          {state.saved?.warning && (
            <p className="text-xs text-[var(--color-muted)]">{state.saved.warning}</p>
          )}
          {state.error?.during === "save" && (
            <p className="text-xs text-[var(--color-bad)]" data-testid="pkg-name-error">
              {tr(
                "upload_pkg_name_failed",
                { reason: state.error.reason },
                "The name was not changed: {reason}",
              )}
            </p>
          )}
        </>
      )}
    </section>
  );
}

/** The field for the package at `path` (a file on the engine's computer). `onRenamed` runs once
 *  the package holds another name, so the screen reads its details again. */
export function PackageNameCard({
  path,
  disabled,
  onRenamed,
}: {
  path: string;
  disabled?: boolean;
  onRenamed?: (path: string) => void;
}) {
  const flow = useMemo(() => createNameFlow({ read: readPkgName, set: setPkgName }), []);
  const state = useSyncExternalStore(flow.subscribe, flow.get, flow.get);
  useEffect(() => {
    void flow.load(path);
  }, [flow, path]);
  // Told once per change, about the package the change was made to (not one picked since).
  const renamedRef = useRef(onRenamed);
  useEffect(() => {
    renamedRef.current = onRenamed;
  });
  const { saved, path: savedPath } = state;
  useEffect(() => {
    if (saved && saved.changed > 0 && savedPath) renamedRef.current?.(savedPath);
  }, [saved, savedPath]);
  return (
    <PackageNameView
      state={state}
      disabled={disabled}
      onValue={flow.setValue}
      onAllLanguages={flow.setAllLanguages}
      onSave={() => void flow.save()}
    />
  );
}
