// "Invite a device": a QR code and link that add this console to another device's PS5Upload. The
// link carries only how to reach the console (its home and Tailscale addresses) and who it is (its
// key, to tell it from another PS5 at the same address); nothing in it lets anyone in. The console
// lets the new device in the way it always does: with "Ask before a new device can connect" on, the
// device asks, and this sheet (any trusted app, or the PS5's own page) answers Allow or Decline.

import { useEffect, useState } from "react";
import { Copy, Check, Loader2 } from "lucide-react";

import { Button, Toggle } from "../../../components";
import { Modal } from "../../../components/Modal";
import { QrCode } from "../../Play/PhonePads";
import { consoleIdentity } from "../../../api/ava1";
import { meshDevice, meshSettings, type MeshState } from "../../../api/mesh";
import { pollMesh, resetMeshCursor } from "../../../state/mesh";
import { useTr } from "../../../state/lang";
import { hostOf } from "../../../lib/addr";
import { humanizeCaught } from "../../../lib/humanizeError";
import { AskRows } from "./TrustedDevices";
import { inviteLink, isTailnetIp, type Invite } from "./model";

/** The sheet's body, from what it knows (render-tested on its own). */
export function InviteView({
  invite,
  loading,
  state,
  busy,
  error,
  copied,
  onCopy,
  onAskFirst,
  onAnswer,
}: {
  invite: Invite;
  loading: boolean;
  state: MeshState | null;
  busy: boolean;
  error: string | null;
  copied: boolean;
  onCopy: () => void;
  onAskFirst: (on: boolean) => void;
  onAnswer: (op: "allow" | "decline", id: string) => void;
}) {
  const tr = useTr();
  const link = inviteLink(invite);
  const asks = state?.asks ?? [];
  const askFirst = !!state?.settings.require_approval;
  return (
    <div className="flex flex-col gap-4 p-4 text-sm" data-testid="consoles-invite">
      <p className="text-[var(--color-muted)]">
        {tr(
          "cs_invite_intro",
          { name: invite.name },
          "Open this on the new phone or computer: scan the code with its camera, or paste the link in PS5Upload › Connection › Add a PS5. It adds {name} there.",
        )}
      </p>
      <div className="flex flex-col items-center gap-3 sm:flex-row sm:items-start">
        <div className="shrink-0 rounded-[var(--radius-field)] bg-white p-2">
          {loading ? (
            <div className="grid h-[176px] w-[176px] place-items-center text-[var(--color-muted)]">
              <Loader2 size={20} className="animate-spin" aria-hidden />
            </div>
          ) : (
            <QrCode text={link} label={tr("cs_invite_qr", undefined, "Invite code")} size={176} testId="consoles-invite-qr" />
          )}
        </div>
        <dl className="grid min-w-0 flex-1 grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-xs">
          <dt className="text-[var(--color-muted)]">{tr("cs_reach_home", undefined, "Home network")}</dt>
          <dd className="truncate font-mono">{invite.home ?? "—"}</dd>
          <dt className="text-[var(--color-muted)]">{tr("cs_reach_tailscale", undefined, "Tailscale")}</dt>
          <dd className="truncate font-mono">{invite.tailnet ?? "—"}</dd>
        </dl>
      </div>
      <div className="flex min-w-0 items-center gap-2">
        <input
          readOnly
          value={link}
          aria-label={tr("cs_invite_link", undefined, "Invite link")}
          className="input min-w-0 flex-1 font-mono text-xs"
          onFocus={(e) => e.currentTarget.select()}
          data-testid="consoles-invite-link"
        />
        <Button size="sm" variant="secondary" leftIcon={copied ? <Check size={13} /> : <Copy size={13} />} onClick={onCopy}>
          {copied ? tr("cs_invite_copied", undefined, "Copied") : tr("cs_invite_copy", undefined, "Copy")}
        </Button>
      </div>
      <p className="text-xs text-[var(--color-muted)]">
        {tr(
          "cs_invite_safe",
          undefined,
          "The link only says where this PS5 is and which one it is. It lets nobody in by itself.",
        )}
      </p>
      {state ? (
        <div className="flex flex-col gap-2 border-t border-[var(--color-border)] pt-3">
          <Toggle
            checked={askFirst}
            disabled={busy}
            onChange={onAskFirst}
            label={tr("mesh_require_approval", undefined, "Ask before a new device can connect")}
            hint={
              askFirst
                ? tr(
                    "cs_invite_ask_on",
                    undefined,
                    "The new device waits for your Allow: here, on any device that uses this PS5, or on the PS5's own page.",
                  )
                : tr(
                    "cs_invite_ask_off",
                    undefined,
                    "Off: any device on your home network gets in without asking. Turn it on to approve the new device yourself.",
                  )
            }
          />
          {askFirst && asks.length === 0 ? (
            <p className="flex items-center gap-2 text-xs text-[var(--color-muted)]" data-testid="consoles-invite-waiting">
              <Loader2 size={12} className="animate-spin" aria-hidden />
              {tr("cs_invite_waiting", undefined, "Waiting for the new device to ask…")}
            </p>
          ) : null}
          <AskRows asks={asks} consoleName={invite.name} busy={busy} onAnswer={onAnswer} />
        </div>
      ) : null}
      {error ? (
        <p role="alert" className="text-xs text-[var(--color-bad)]">
          {error}
        </p>
      ) : null}
    </div>
  );
}

export function InviteSheet({
  open,
  onClose,
  name,
  host,
  tailnetIp,
  state,
}: {
  open: boolean;
  onClose: () => void;
  name: string;
  host: string;
  tailnetIp: string | null;
  state: MeshState | null;
}) {
  const tr = useTr();
  const [key, setKey] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    if (!open || !host) return;
    let live = true;
    setLoading(true);
    void consoleIdentity(host).then((id) => {
      if (!live) return;
      setKey(id?.key ?? null);
      setLoading(false);
    });
    return () => {
      live = false;
    };
  }, [open, host]);

  // While the sheet is open the console's list is read often: a new device asks within seconds.
  useEffect(() => {
    if (!open || !host) return;
    const id = window.setInterval(() => {
      if (!document.hidden) void pollMesh(host);
    }, 3000);
    return () => window.clearInterval(id);
  }, [open, host]);

  const bare = hostOf(host);
  const invite: Invite = {
    name,
    home: bare && !isTailnetIp(bare) ? bare : null,
    tailnet: tailnetIp ?? (isTailnetIp(bare) ? bare : null),
    key,
  };
  const run = async (f: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await f();
      resetMeshCursor(host);
      await pollMesh(host);
    } catch (e) {
      setError(humanizeCaught(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Modal open={open} onClose={onClose} title={tr("cs_invite_title", { name }, "Invite a device to {name}")} size="md">
      <InviteView
        invite={invite}
        loading={loading}
        state={state}
        busy={busy}
        error={error}
        copied={copied}
        onCopy={() => {
          // No clipboard on a plain-http page (the console's web page on the LAN): say so.
          const failed = () =>
            setError(
              tr("cs_invite_copy_failed", undefined, "This page can't copy. Select the invite link above and copy it yourself."),
            );
          if (!navigator.clipboard?.writeText) {
            failed();
            return;
          }
          void navigator.clipboard
            .writeText(inviteLink(invite))
            .then(() => {
              setCopied(true);
              window.setTimeout(() => setCopied(false), 2000);
            })
            .catch(failed);
        }}
        onAskFirst={(on) => void run(() => meshSettings(host, { require_approval: on }))}
        onAnswer={(op, id) => void run(() => meshDevice(host, op, id))}
      />
    </Modal>
  );
}
