import { useState } from "react";
import {
  CheckCircle2,
  Moon,
  ChevronDown,
  ChevronRight,
  Loader2,
  AlertTriangle,
} from "lucide-react";
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
  wakeSetupStage,
  type PowerState,
  type WakeUi,
} from "../../lib/wakeState";

// Example key values shown as input placeholders — illustrative hex, not
// translatable copy, so kept as constants rather than i18n strings.
const EXAMPLE_REGIST_KEY = "35393637626264330000000000000000";
const EXAMPLE_RP_KEY = "1395c8cc7eca16fe982eb22e527ba3da";

/** The console-side settings wake depends on.
 *
 *  ps5upload turns Remote Play on itself during setup (the payload can write
 *  that one), but the two rest-mode settings live in a menu we cannot reach,
 *  and wake fails SILENTLY without them. So this stays — collapsed, because
 *  it is reference material, not a step. It used to open by default and was
 *  the first thing anyone saw, which made a one-click feature look like a
 *  chore.
 */
function Requirements() {
  const tr = useTr();
  const [open, setOpen] = useState(false);
  return (
    <div className="rounded border border-[var(--color-border)] bg-[var(--color-surface)]">
      <button
        type="button"
        className="flex w-full items-center gap-1.5 p-2 text-left text-xs text-[var(--color-muted)] hover:text-[var(--color-fg)]"
        onClick={() => setOpen((v) => !v)}
      >
        {open ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
        {tr(
          "power_wake_req_title_v2",
          undefined,
          "What this needs switched on at the console",
        )}
      </button>
      {open ? (
        <ol className="flex flex-col gap-2 px-2 pb-2">
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
  power: PowerState;
  ui: WakeUi;
}

/** Getting a console ready to wake — and to wake straight into your user.
 *
 *  One button does the whole thing. ps5upload runs code on the console, so it
 *  reads the PSN account id from the registry, has Sony's own API mint a
 *  pairing PIN, and runs the Remote Play registration itself; the credential
 *  and both session keys come back from that one call.
 *
 *  This panel used to lead with a field for pasting the wake code by hand and
 *  demote pairing to a link captioned "may not work on all consoles" — which
 *  pointed people at Chiaki to fetch secrets this tool can mint. Pairing is
 *  the path; typing keys is the fallback for when a console refuses.
 */
export default function WakeSetup({
  host,
  addr,
  profileId,
  credential,
  power,
  ui,
}: WakeSetupProps) {
  const tr = useTr();
  const setWakeCredential = useRosterStore((st) => st.setWakeCredential);
  const setWakeSessionKeys = useRosterStore((st) => st.setWakeSessionKeys);

  const [pairing, setPairing] = useState(false);
  const [pairError, setPairError] = useState<string | null>(null);
  /** Console nickname from the last successful pairing — proof we talked to
   *  the right box. Only known just after pairing, so absence is not a fault. */
  const [pairedAs, setPairedAs] = useState<string | null>(null);
  const [showManual, setShowManual] = useState(false);

  // Manual fallback drafts.
  const [draft, setDraft] = useState("");
  const [registDraft, setRegistDraft] = useState("");
  const [rpDraft, setRpDraft] = useState("");
  const draftValid = isValidWakeCredential(draft);
  const sessionKeysValid =
    isValidSessionKey(registDraft) && isValidSessionKey(rpDraft);

  const stage = wakeSetupStage(power, !!credential, ui.canSignIn, pairing);

  function saveCredential() {
    if (!profileId || !draftValid) return;
    setWakeCredential(profileId, draft.trim());
    setDraft("");
    setShowManual(false);
    setPairError(null);
    audit("system_wake", withConsolePrefix(host, "wake code saved"));
  }

  function saveSessionKeys() {
    if (!profileId || !sessionKeysValid) return;
    setWakeSessionKeys(profileId, registDraft, rpDraft);
    // The wake credential derives from the regist key, so storing the keys is
    // enough to also enable the plain wake — set it so both paths work.
    const derived = credentialFromRegistKeyHex(registDraft);
    if (derived) setWakeCredential(profileId, derived);
    setRegistDraft("");
    setRpDraft("");
    setShowManual(false);
    audit("system_wake", withConsolePrefix(host, "sign-in keys saved"));
  }

  async function autoSetup() {
    if (!profileId) return;
    setPairing(true);
    setPairError(null);
    try {
      const r = await pairForWake(host, addr);
      setWakeCredential(profileId, r.credential);
      // Both keys arrive from the same handshake, already padded to the
      // 16-byte form the session code parses.
      if (isValidSessionKey(r.regist_key) && isValidSessionKey(r.rp_key)) {
        setWakeSessionKeys(profileId, r.regist_key, r.rp_key);
      }
      setPairedAs(r.nickname || null);
      pushNotification(
        "success",
        tr("power_wake_ready_toast", undefined, "This console can now be woken"),
        {
          body: tr(
            "power_wake_ready_body",
            undefined,
            "Set up. Wake and automatic sign-in will work from standby, as long as the console settings stay enabled.",
          ),
        },
      );
      audit("system_wake", withConsolePrefix(host, "paired for wake"));
    } catch (e) {
      // The engine's pairing errors are already specific and actionable
      // ("no signed-in user with an activated PSN account…"), so show them
      // rather than a house-style "setup failed".
      setPairError(e instanceof Error ? e.message : String(e));
      setShowManual(true);
    } finally {
      setPairing(false);
    }
  }

  const setupButton = (labelKey: string, fallback: string) => (
    <Button
      variant="primary"
      size="sm"
      disabled={pairing || !profileId}
      onClick={() => void autoSetup()}
    >
      {tr(labelKey, undefined, fallback)}
    </Button>
  );

  return (
    <div className="mt-2 flex flex-col gap-2">
      <div className="flex items-center gap-1.5 text-xs font-semibold">
        <Moon size={11} />
        {tr("power_wake_setup_title", undefined, "Set up wake-from-standby")}
      </div>

      {stage === "working" ? (
        <div className="flex items-start gap-2 text-xs">
          <Loader2
            size={12}
            className="mt-0.5 shrink-0 animate-spin text-[var(--color-accent)]"
          />
          <span className="flex flex-col gap-0.5">
            <span>
              {tr("power_wake_working", undefined, "Setting this console up…")}
            </span>
            <span className="text-[var(--color-muted)]">
              {tr(
                "power_wake_working_detail",
                undefined,
                "Turning on Remote Play, reading your account from the console, and registering. Nothing needed from you.",
              )}
            </span>
          </span>
        </div>
      ) : null}

      {stage === "done" ? (
        <div className="flex items-center gap-2 text-xs">
          <CheckCircle2
            size={12}
            className="shrink-0 text-[var(--color-good)]"
          />
          <span>
            {tr(
              "power_wake_signin_ready",
              undefined,
              "Wakes straight into your user",
            )}
          </span>
          {pairedAs ? (
            <span className="text-[var(--color-muted)]">
              {tr("power_wake_paired_with", { name: pairedAs }, "— {name}")}
            </span>
          ) : null}
          <button
            type="button"
            className="text-[var(--color-accent)] hover:underline"
            onClick={() => void autoSetup()}
          >
            {tr("power_wake_redo", undefined, "Set up again")}
          </button>
        </div>
      ) : null}

      {stage === "wake-only" ? (
        <div className="flex flex-col gap-2">
          <div className="flex items-center gap-2 text-xs">
            <CheckCircle2
              size={12}
              className="shrink-0 text-[var(--color-good)]"
            />
            <span>
              {tr(
                "power_wake_ready_userselect",
                undefined,
                "Wake is set up — it stops at the user-select screen",
              )}
            </span>
          </div>
          {power === "awake" ? (
            <div className="flex flex-wrap items-center gap-2 text-xs">
              {setupButton(
                "power_wake_upgrade",
                "Also sign in automatically",
              )}
              <span className="text-[var(--color-muted)]">
                {tr(
                  "power_wake_upgrade_hint",
                  undefined,
                  "One click — ps5upload fetches the keys from the console.",
                )}
              </span>
            </div>
          ) : null}
        </div>
      ) : null}

      {stage === "offer" ? (
        <div className="flex flex-col gap-2">
          <span className="text-xs text-[var(--color-muted)]">
            {tr(
              "power_wake_offer_hint",
              undefined,
              "ps5upload can set this up for you — it reads what it needs from the console. Nothing to look up, and no other apps needed.",
            )}
          </span>
          <div>
            {setupButton("power_wake_setup_cta", "Set up wake & sign-in")}
          </div>
        </div>
      ) : null}

      {stage === "unreachable" ? (
        <span className="text-xs text-[var(--color-muted)]">
          {tr(
            "power_wake_needs_awake",
            undefined,
            "Turn the console on to set this up — pairing registers over the network, so it only works while the console is awake. You only have to do it once.",
          )}
        </span>
      ) : null}

      {pairError ? (
        <div className="flex items-start gap-2 text-xs text-[var(--color-bad)]">
          <AlertTriangle size={12} className="mt-0.5 shrink-0" />
          <span>{pairError}</span>
        </div>
      ) : null}

      <Requirements />

      <ManualEntry
        show={showManual}
        setShow={setShowManual}
        draft={draft}
        setDraft={setDraft}
        draftValid={draftValid}
        onSaveCredential={saveCredential}
        registDraft={registDraft}
        setRegistDraft={setRegistDraft}
        rpDraft={rpDraft}
        setRpDraft={setRpDraft}
        sessionKeysValid={sessionKeysValid}
        onSaveSessionKeys={saveSessionKeys}
        canSave={!!profileId}
      />
    </div>
  );
}

/** The escape hatch: type the credential, or both session keys, by hand.
 *
 *  Only for a console that refuses to pair. It is collapsed by default and
 *  opens by itself when pairing fails, so the fallback appears exactly when
 *  it becomes relevant instead of competing with the button that works.
 */
function ManualEntry(props: {
  show: boolean;
  setShow: (v: boolean) => void;
  draft: string;
  setDraft: (v: string) => void;
  draftValid: boolean;
  onSaveCredential: () => void;
  registDraft: string;
  setRegistDraft: (v: string) => void;
  rpDraft: string;
  setRpDraft: (v: string) => void;
  sessionKeysValid: boolean;
  onSaveSessionKeys: () => void;
  canSave: boolean;
}) {
  const tr = useTr();
  const {
    show,
    setShow,
    draft,
    setDraft,
    draftValid,
    onSaveCredential,
    registDraft,
    setRegistDraft,
    rpDraft,
    setRpDraft,
    sessionKeysValid,
    onSaveSessionKeys,
    canSave,
  } = props;
  return (
    <div className="border-t border-[var(--color-border)] pt-2">
      <button
        type="button"
        className="flex items-center gap-1.5 text-xs text-[var(--color-muted)] hover:text-[var(--color-fg)]"
        onClick={() => setShow(!show)}
      >
        {show ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
        {tr("power_wake_manual", undefined, "Enter keys manually")}
      </button>
      {show ? (
        <div className="mt-2 flex flex-col gap-3">
          <span className="text-xs text-[var(--color-muted)]">
            {tr(
              "power_wake_manual_hint",
              undefined,
              "Only needed if this console refuses to pair. Fill in either part: the wake code alone wakes to user-select; both session keys also sign your user in.",
            )}
          </span>

          <div className="flex flex-col gap-1">
            <label className="text-xs text-[var(--color-muted)]">
              {tr("power_wake_code_label_v2", undefined, "Wake code")}
            </label>
            <div className="flex flex-wrap items-center gap-2">
              <input
                className="input py-1 text-xs"
                style={{ width: "auto", minWidth: "12rem" }}
                inputMode="numeric"
                placeholder={tr(
                  "power_wake_code_placeholder",
                  undefined,
                  "e.g. 1499970515",
                )}
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && draftValid) onSaveCredential();
                }}
              />
              <Button
                variant="secondary"
                size="sm"
                disabled={!draftValid || !canSave}
                onClick={onSaveCredential}
              >
                {tr("power_wake_code_save", undefined, "Save")}
              </Button>
            </div>
            {draft && !draftValid ? (
              <span className="text-xs text-[var(--color-bad)]">
                {tr(
                  "power_wake_code_invalid",
                  undefined,
                  "That doesn't look like a wake code — it should be a number, not the raw key.",
                )}
              </span>
            ) : null}
          </div>

          <div className="flex flex-col gap-2">
            <span className="text-xs text-[var(--color-muted)]">
              {tr(
                "power_wake_signin_keys_label",
                undefined,
                "Session keys (both 32 hex characters)",
              )}
            </span>
            <label className="flex flex-col gap-1 text-xs">
              <span className="text-[var(--color-muted)]">
                {tr(
                  "power_wake_signin_regist",
                  undefined,
                  "Registration key (hex)",
                )}
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
              <Button
                variant="secondary"
                size="sm"
                disabled={!sessionKeysValid || !canSave}
                onClick={onSaveSessionKeys}
              >
                {tr("power_wake_signin_save", undefined, "Save sign-in keys")}
              </Button>
            </div>
          </div>
        </div>
      ) : null}
    </div>
  );
}
