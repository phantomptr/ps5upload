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
  isValidSessionKey,
  credentialFromRegistKeyHex,
  type WakeUi,
} from "../../lib/wakeState";

// Example key values shown as input placeholders — illustrative hex, not
// translatable copy, so kept as constants rather than i18n strings.
const EXAMPLE_REGIST_KEY = "35393637626264330000000000000000";
const EXAMPLE_RP_KEY = "1395c8cc7eca16fe982eb22e527ba3da";

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
  const setWakeSessionKeys = useRosterStore((st) => st.setWakeSessionKeys);

  const [draft, setDraft] = useState("");
  const [editing, setEditing] = useState(false);
  const [pairing, setPairing] = useState(false);
  const [pairError, setPairError] = useState<string | null>(null);

  // Advanced: the two session keys that let the wake sign the user in.
  const [showSignIn, setShowSignIn] = useState(false);
  const [registDraft, setRegistDraft] = useState("");
  const [rpDraft, setRpDraft] = useState("");
  const sessionKeysValid = isValidSessionKey(registDraft) && isValidSessionKey(rpDraft);

  function saveSessionKeys() {
    if (!profileId || !sessionKeysValid) return;
    setWakeSessionKeys(profileId, registDraft, rpDraft);
    // The wake credential derives from the regist key, so storing the keys is
    // enough to also enable the plain wake — set it so both paths work.
    const derived = credentialFromRegistKeyHex(registDraft);
    if (derived) setWakeCredential(profileId, derived);
    setRegistDraft("");
    setRpDraft("");
    setShowSignIn(false);
  }

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
        {ui.canSignIn ? (
          <div className="flex items-center gap-2 text-xs text-[var(--color-muted)]">
            <CheckCircle2 size={12} className="shrink-0 text-[var(--color-good)]" />
            {tr("power_wake_signin_ready", undefined, "Wakes straight into your user")}
          </div>
        ) : (
          <SignInSection
            show={showSignIn}
            setShow={setShowSignIn}
            registDraft={registDraft}
            setRegistDraft={setRegistDraft}
            rpDraft={rpDraft}
            setRpDraft={setRpDraft}
            valid={sessionKeysValid}
            onSave={saveSessionKeys}
            canSave={!!profileId}
          />
        )}
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

      <SignInSection
        show={showSignIn}
        setShow={setShowSignIn}
        registDraft={registDraft}
        setRegistDraft={setRegistDraft}
        rpDraft={rpDraft}
        setRpDraft={setRpDraft}
        valid={sessionKeysValid}
        onSave={saveSessionKeys}
        canSave={!!profileId}
      />
    </div>
  );
}

/** Optional: the two session keys that let a wake sign the user in, so the
 *  console lands on the home screen rather than user-select. A power-user
 *  step — both keys come together from a pairing and have to be harvested —
 *  so it is tucked behind a disclosure and never in the main path. */
function SignInSection(props: {
  show: boolean;
  setShow: (v: boolean) => void;
  registDraft: string;
  setRegistDraft: (v: string) => void;
  rpDraft: string;
  setRpDraft: (v: string) => void;
  valid: boolean;
  onSave: () => void;
  canSave: boolean;
}) {
  const tr = useTr();
  const {
    show, setShow, registDraft, setRegistDraft, rpDraft, setRpDraft, valid, onSave, canSave,
  } = props;
  return (
    <div className="mt-1 border-t border-[var(--color-border)] pt-2">
      <button
        type="button"
        className="flex items-center gap-1.5 text-xs text-[var(--color-muted)] hover:text-[var(--color-fg)]"
        onClick={() => setShow(!show)}
      >
        {show ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
        {tr("power_wake_signin_setup", undefined,
          "Advanced: wake straight into your user (sign in)")}
      </button>
      {show ? (
        <div className="mt-2 flex flex-col gap-2">
          <span className="text-xs text-[var(--color-muted)]">
            {tr("power_wake_signin_setup_hint", undefined,
              "A plain wake stops at user-select. With your console's two session keys (registration key and RP-Key, both 32 hex characters), the wake also signs your user in. Chiaki users have both in their registered-console settings.")}
          </span>
          <label className="flex flex-col gap-1 text-xs">
            <span className="text-[var(--color-muted)]">
              {tr("power_wake_signin_regist", undefined, "Registration key (hex)")}
            </span>
            <input
              className="input py-1 font-mono text-xs"
              spellCheck={false}
              placeholder={EXAMPLE_REGIST_KEY}
              value={registDraft}
              onChange={(e) => setRegistDraft(e.target.value)}
            />
          </label>
          <label className="flex flex-col gap-1 text-xs">
            <span className="text-[var(--color-muted)]">
              {tr("power_wake_signin_rpkey", undefined, "RP-Key (hex)")}
            </span>
            <input
              className="input py-1 font-mono text-xs"
              spellCheck={false}
              placeholder={EXAMPLE_RP_KEY}
              value={rpDraft}
              onChange={(e) => setRpDraft(e.target.value)}
            />
          </label>
          <div>
            <Button variant="secondary" size="sm" disabled={!valid || !canSave} onClick={onSave}>
              {tr("power_wake_signin_save", undefined, "Save sign-in keys")}
            </Button>
          </div>
        </div>
      ) : null}
    </div>
  );
}
