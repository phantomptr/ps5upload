import { useState } from "react";
import { CheckCircle2, Moon, ChevronDown, ChevronRight } from "lucide-react";
import { Button } from "../../components";
import { useTr } from "../../state/lang";
import { pairForWake } from "../../api/ps5";
import { useRosterStore, withConsolePrefix } from "../../state/roster";
import { pushNotification } from "../../state/notifications";
import { audit } from "../../state/auditLog";
import {
  WAKE_REQUIREMENTS,
  isValidWakeCredential,
  type WakeUi,
} from "../../lib/wakeState";

/** The three console settings that must be on, or wake fails silently.
 *
 *  Shown expanded during setup and collapsed once wake is configured — the
 *  settings are the part people forget, and the part that produced hours of
 *  "the packet is fine but nothing wakes". */
function Requirements({ defaultOpen }: { defaultOpen: boolean }) {
  const tr = useTr();
  const [open, setOpen] = useState(defaultOpen);
  return (
    <div className="rounded border border-[var(--color-border)] bg-[var(--color-surface)] p-2">
      <button
        type="button"
        className="flex w-full items-center gap-1.5 text-left text-xs font-semibold"
        onClick={() => setOpen((v) => !v)}
      >
        {open ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
        {tr("power_wake_req_title", undefined,
          "Enable these on the console first — wake fails silently without them")}
      </button>
      {open ? (
        <ol className="mt-2 flex flex-col gap-2">
          {WAKE_REQUIREMENTS.map((r, i) => (
            <li key={`${r.labelKey}-${i}`} className="flex gap-2 text-xs">
              <span className="text-[var(--color-muted)]">{i + 1}.</span>
              <span className="flex flex-col">
                <span className="font-medium">
                  {tr(r.labelKey, undefined, r.labelFallback)}
                </span>
                <span className="text-[var(--color-muted)]">
                  {tr(r.pathKey, undefined, r.pathFallback)}
                </span>
              </span>
            </li>
          ))}
        </ol>
      ) : null}
    </div>
  );
}

interface WakeSetupProps {
  host: string;
  addr: string;
  profileId: string | null;
  credential: string;
  ui: WakeUi;
}

/** Everything for getting a console ready to wake: the console-side
 *  settings, and this console's wake code.
 *
 *  Credential entry is the primary path — pasting the number always works,
 *  unlike registering over the network, which the console refuses on some
 *  setups. Automatic setup is offered as a convenience that may fail, never
 *  as the only way in. */
export default function WakeSetup({
  host,
  addr,
  profileId,
  credential,
  ui,
}: WakeSetupProps) {
  const tr = useTr();
  const setWakeCredential = useRosterStore((st) => st.setWakeCredential);

  const [draft, setDraft] = useState("");
  const [editing, setEditing] = useState(false);
  const [pairing, setPairing] = useState(false);
  const [pairError, setPairError] = useState<string | null>(null);

  const draftValid = isValidWakeCredential(draft);

  function save() {
    if (!profileId || !draftValid) return;
    setWakeCredential(profileId, draft.trim());
    setDraft("");
    setEditing(false);
    setPairError(null);
    audit("system_wake", withConsolePrefix(host, "wake code saved"));
  }

  async function autoSetup() {
    if (!profileId) return;
    setPairing(true);
    setPairError(null);
    try {
      const r = await pairForWake(host, addr);
      setWakeCredential(profileId, r.credential);
      pushNotification(
        "success",
        tr("power_wake_ready_toast", undefined, "This console can now be woken"),
        {
          body: tr("power_wake_ready_body", undefined,
            "Set up. Wake will work from standby, as long as the console settings above stay enabled."),
        },
      );
      audit("system_wake", withConsolePrefix(host, "paired for wake"));
    } catch (e) {
      setPairError(e instanceof Error ? e.message : String(e));
    } finally {
      setPairing(false);
    }
  }

  // Configured: a compact confirmation, with the requirements one click away
  // as a reminder and a way to re-enter the code if it was wrong.
  if (credential && !editing) {
    return (
      <div className="mt-2 flex flex-col gap-2">
        <div className="flex items-center gap-2 text-xs">
          <CheckCircle2 size={12} className="shrink-0 text-[var(--color-good)]" />
          <span className="text-[var(--color-muted)]">
            {tr("power_wake_ready", undefined, "Wake-from-standby is set up")}
          </span>
          <button
            type="button"
            className="text-[var(--color-accent)] hover:underline"
            onClick={() => {
              setDraft(credential);
              setEditing(true);
            }}
          >
            {tr("power_wake_change", undefined, "Change")}
          </button>
        </div>
        <Requirements defaultOpen={false} />
      </div>
    );
  }

  return (
    <div className="mt-2 flex flex-col gap-2">
      <div className="flex items-center gap-1.5 text-xs font-semibold">
        <Moon size={11} />
        {tr("power_wake_setup_title", undefined, "Set up wake-from-standby")}
      </div>

      <Requirements defaultOpen={true} />

      <div className="flex flex-col gap-1">
        <label className="text-xs text-[var(--color-muted)]">
          {tr("power_wake_code_label", undefined, "Then enter this console's wake code")}
        </label>
        <div className="flex flex-wrap items-center gap-2">
          <input
            className="input py-1 text-xs"
            style={{ width: "auto", minWidth: "12rem" }}
            inputMode="numeric"
            placeholder={tr("power_wake_code_placeholder", undefined, "e.g. 1499970515")}
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && draftValid) save();
            }}
          />
          <Button
            variant="secondary"
            size="sm"
            disabled={!draftValid || !profileId}
            onClick={save}
          >
            {tr("power_wake_code_save", undefined, "Save")}
          </Button>
          {editing ? (
            <button
              type="button"
              className="text-xs text-[var(--color-muted)] hover:underline"
              onClick={() => {
                setDraft("");
                setEditing(false);
              }}
            >
              {tr("cancel", undefined, "Cancel")}
            </button>
          ) : null}
        </div>
        {draft && !draftValid ? (
          <span className="text-xs text-[var(--color-bad)]">
            {tr("power_wake_code_invalid", undefined,
              "That doesn't look like a wake code — it should be a number, not the raw key.")}
          </span>
        ) : (
          <span className="text-xs text-[var(--color-muted)]">
            {tr("power_wake_code_hint", undefined,
              "Your console's Remote Play registration key as a number. Chiaki users: it's the registered console's key.")}
          </span>
        )}
      </div>

      {/* Automatic setup: convenient when it works, refused by some consoles.
          Offered only while the console is awake, since it registers over the
          network. Never the only path — the code entry above always works. */}
      {ui.canAutoSetup ? (
        <div className="flex flex-wrap items-center gap-2 text-xs">
          <button
            type="button"
            className="text-[var(--color-accent)] hover:underline disabled:opacity-50"
            disabled={pairing || !profileId}
            onClick={() => void autoSetup()}
          >
            {pairing
              ? tr("power_wake_auto_working", undefined, "Setting up…")
              : tr("power_wake_auto_try", undefined, "Or try automatic setup")}
          </button>
          <span className="text-[var(--color-muted)]">
            {tr("power_wake_auto_hint", undefined, "may not work on all consoles")}
          </span>
        </div>
      ) : null}

      {pairError ? (
        <span className="text-xs text-[var(--color-bad)]">
          {tr("power_wake_auto_failed", undefined,
            "Automatic setup didn't work — enter the wake code above instead.")}
          {" "}
          {pairError}
        </span>
      ) : null}
    </div>
  );
}
