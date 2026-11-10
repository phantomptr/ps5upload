import { isIOS } from "../../lib/platform";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Link2, Radar, ScanLine, Users } from "lucide-react";

import { Button, Input } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import { linkFromSocket, normalizeCode, parseJoinLink, type JoinLink } from "../../lib/joinProtocol";
import { JoinView } from "../../join/JoinView";
import { lastName, rememberName } from "../../join/guestName";
import { useTr } from "../../state/lang";
import { useJoinPresence } from "../../state/joinPresence";
import { activeJoin, useJoinStore } from "../../state/joinSession";
import { useRemotePlayStore } from "../../state/remotePlay";
import { holdPageStill, lockLandscape, setWindowFullscreen, unlockOrientation } from "./immersive";
import { useConnectionStore } from "../../state/connection";
import { deviceName, meshFor, nearbyJoins, useMeshStore } from "../../state/mesh";
import { ownJoinWs } from "./playTogetherState";

type Tr = ReturnType<typeof useTr>;

/** What a link looks like (an address, not words: the same in every language). */
const EXAMPLE_LINK = "http://192.168.1.20:19113/join#i=…";
const EXAMPLE_CODE = "K7M-2QX";
type Confirm = ReturnType<typeof useConfirm>["confirm"];

/** The name a link's host goes by: its own, else its address. */
export function linkHost(link: JoinLink): string {
  if (link.name) return link.name;
  try {
    return new URL(link.origin).hostname;
  } catch {
    return link.origin;
  }
}

/** Is the app's own Remote Play streaming (any console)? Its console's name, or null. */
function ownStream(): string | null {
  const s = useRemotePlayStore.getState();
  if (!s.stream || s.phase === null || s.phase === "ended") return null;
  return s.target?.name || s.target?.host || "";
}

/**
 * One thing at a time: before joining `link`, ask to stop the app's own Remote Play or to leave
 * the friend's game it is in. True when it may go ahead (whatever ran is stopped by then).
 */
export async function okToJoin(tr: Tr, confirm: Confirm, link: JoinLink): Promise<boolean> {
  const other = linkHost(link);
  const consoleName = ownStream();
  const continueLabel = tr("join_confirm_continue", undefined, "Continue");
  if (consoleName !== null) {
    const ok = await confirm({
      title: tr("join_confirm_stop_rp", { host: other, console: consoleName }, `Joining ${other}'s game stops Remote Play on ${consoleName}. Continue?`),
      confirmLabel: continueLabel,
    });
    if (!ok) return false;
    useRemotePlayStore.getState().stop();
    for (let i = 0; i < 50 && useRemotePlayStore.getState().stream; i++) {
      await new Promise((r) => window.setTimeout(r, 100));
    }
    return true;
  }
  const joined = activeJoin();
  if (joined) {
    const host = joined.host || other;
    const ok = await confirm({
      title: tr("join_confirm_switch", { host, other }, `Leave ${host}'s game and join ${other}?`),
      confirmLabel: continueLabel,
    });
    if (!ok) return false;
    useJoinStore.getState().leave();
  }
  return true;
}

/**
 * One thing at a time, the other way: before the app starts its own Remote Play, ask to leave
 * the friend's game it is in. True when it may go ahead.
 */
export async function okToStartPlay(tr: Tr, confirm: Confirm): Promise<boolean> {
  const joined = activeJoin();
  if (!joined) return true;
  const host = joined.host || tr("join_host_fallback", undefined, "your friend");
  const ok = await confirm({
    title: tr("join_confirm_leave_start", { host }, `Starting Play leaves ${host}'s game. Continue?`),
    confirmLabel: tr("join_confirm_continue", undefined, "Continue"),
  });
  if (ok) useJoinStore.getState().leave();
  return ok;
}

/**
 * Play › Join a friend: paste the link a friend's Play Together shows (or open one sent to the
 * app, /play?join=…) and play their PS5 as Player 2, 3 or 4, in the same picture box as Play.
 * The app decodes with WebCodecs (a secure page), the lowest-delay path.
 */
export function JoinAFriend({ pending, onPendingDone }: { pending?: string | null; onPendingDone?: () => void }) {
  // The keyboard is the game's only while this shows (join/session.ts).
  useEffect(() => {
    useJoinPresence.setState({ onScreen: true });
    return () => useJoinPresence.setState({ onScreen: false });
  }, []);
  const tr = useTr();
  const session = useJoinStore((s) => s.session);
  const lastLink = useJoinStore((s) => s.link);
  const { confirm, dialog } = useConfirm();
  const [text, setText] = useState(pending ?? "");
  const [codeText, setCodeText] = useState("");
  // The last name used to join, wherever it was typed.
  const [name, setName] = useState(() => lastName());
  // An invite opened in the app (a tapped link): just the name and Join, not the whole form.
  const [invite, setInvite] = useState<JoinLink | null>(() => (pending ? parseJoinLink(pending) : null));
  const inviteForm = useRef<HTMLFormElement>(null);
  const [bad, setBad] = useState<"link" | "code" | null>(null);
  const [scanning, setScanning] = useState(false);
  const busy = useRef(false);
  // Friends hosting Play Together on this network (mesh presence of the connected console).
  const host = useConnectionStore((s) => s.host);
  const meshState = useMeshStore((s) => (host ? meshFor(s, host).state : null));
  const nearby = useMemo(() => nearbyJoins(meshState, ownJoinWs()), [meshState]);

  const go = useCallback(
    async (link: JoinLink, who: string) => {
      if (busy.current) return;
      // Remembered whichever way the guest joins: the next invite has it filled in.
      const clean = rememberName(who);
      if (!clean) return;
      busy.current = true;
      try {
        if (!(await okToJoin(tr, confirm, link))) return;
        setInvite(null);
        useJoinStore.getState().join(link, clean);
      } finally {
        busy.current = false;
      }
    },
    [tr, confirm],
  );

  /** The link if one is pasted, else the short code (matched to a host playing nearby). */
  const join = useCallback(
    async (raw: string, code: string, who: string) => {
      if (raw.trim()) {
        const link = parseJoinLink(raw);
        if (!link) return setBad("link");
        return go(link, who);
      }
      const c = normalizeCode(code);
      const hit = c ? nearby.find((n) => normalizeCode(n.join.code) === c) : undefined;
      const link = hit ? linkFromSocket(hit.join.ws, hit.join.host_name, c) : null;
      if (!link) return setBad("code");
      return go(link, who);
    },
    [go, nearby],
  );

  // A link opened in the app: the invite step (the name, filled in when one was used before,
  // and Join). One tap joins; a bad link lands in the form with its error.
  useEffect(() => {
    if (!pending) return;
    setText(pending);
    onPendingDone?.();
    const link = parseJoinLink(pending);
    setInvite(link);
    setBad(link ? null : "link");
    // Once per link.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pending]);

  // The invite step starts on its name field when it is empty, else on Join.
  const inviteShown = !!invite;
  useEffect(() => {
    if (!inviteShown) return;
    const id = name.trim() ? "join-invite-go" : "join-invite-name";
    inviteForm.current?.querySelector<HTMLElement>(`[data-testid="${id}"]`)?.focus();
    // Only when the step opens, not as the name is typed.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [inviteShown]);

  // An invite opened while in a game shows too: Join asks to leave that game first (okToJoin).
  if (invite) {
    const who = linkHost(invite);
    return (
      <div className="space-y-4" data-testid="join-invite">
        <form
          ref={inviteForm}
          className="space-y-3 rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface-raised)] p-4"
          onSubmit={(e) => {
            e.preventDefault();
            void go(invite, name);
          }}
        >
          <div className="flex items-center gap-2">
            <Users size={16} />
            <h3 className="text-sm font-semibold" data-testid="join-invite-title">
              {tr("join_invite_title", { host: who }, `Join ${who}'s game`)}
            </h3>
          </div>
          <p className="text-sm text-[var(--color-muted)]">
            {tr("join_title_body", { host: who }, `Play on ${who}'s PS5 as Player 2, 3 or 4.`)}
          </p>
          <Input
            label={tr("join_name_label", undefined, "Your name")}
            hint={tr("join_name_hint", undefined, "Your friend sees this when you ask to join.")}
            value={name}
            maxLength={32}
            autoComplete="nickname"
            onChange={(e) => setName(e.target.value)}
            data-testid="join-invite-name"
          />
          <div className="flex flex-wrap gap-2">
            <Button
              type="submit"
              variant="primary"
              disabled={!name.trim()}
              data-testid="join-invite-go"
            >
              {tr("join_button", undefined, "Join")}
            </Button>
            <Button type="button" variant="secondary" onClick={() => setInvite(null)} data-testid="join-invite-cancel">
              {tr("join_cancel", undefined, "Cancel")}
            </Button>
          </div>
        </form>
        {dialog}
      </div>
    );
  }

  if (session) {
    return (
      <>
        <JoinView
          session={session}
          inApp
          onLeave={() => useJoinStore.getState().clear()}
          onAgain={() => {
            const link = lastLink;
            useJoinStore.getState().clear();
            if (link) useJoinStore.getState().join(link, session.opts.name);
          }}
          onFullscreen={appFullscreen}
        />
        {dialog}
      </>
    );
  }

  const canScan = scanAvailable();
  return (
    <div className="space-y-4" data-testid="join-a-friend">
      <form
        className="space-y-3 rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface-raised)] p-4"
        onSubmit={(e) => {
          e.preventDefault();
          void join(text, codeText, name);
        }}
      >
        <div className="flex items-center gap-2">
          <Users size={16} />
          <h3 className="text-sm font-semibold">{tr("play_mode_friend", undefined, "Join a friend")}</h3>
        </div>
        <p className="text-sm text-[var(--color-muted)]">
          {tr(
            "join_app_intro",
            undefined,
            "Paste the link your friend's Play Together shows. You play their PS5 as Player 2, 3 or 4, with your controller or the on-screen one.",
          )}
        </p>
        <Input
          label={tr("join_name_label", undefined, "Your name")}
          hint={tr("join_name_hint", undefined, "Your friend sees this when you ask to join.")}
          value={name}
          maxLength={32}
          onChange={(e) => setName(e.target.value)}
          data-testid="join-app-name"
        />
        {/* Scan QR sits beside the field, not inside it: a long link never runs under it. */}
        <div className="flex items-start gap-2">
          <div className="min-w-0 flex-1">
            <Input
              label={tr("join_link_label", undefined, "Your friend's link")}
              value={text}
              onChange={(e) => {
                setText(e.target.value);
                setBad(null);
              }}
              placeholder={EXAMPLE_LINK}
              leftIcon={<Link2 size={14} />}
              error={bad === "link" ? tr("join_bad_link_v2", undefined, "That isn't a Play Together invite link.") : undefined}
              data-testid="join-link"
            />
          </div>
          {canScan && (
            <Button
              type="button"
              className="mt-5"
              leftIcon={<ScanLine size={14} />}
              onClick={() => setScanning(true)}
              data-testid="join-scan"
            >
              {tr("join_scan", undefined, "Scan QR")}
            </Button>
          )}
        </div>
        <Input
          label={tr("join_code_label", undefined, "Or the short code")}
          value={codeText}
          onChange={(e) => {
            setCodeText(e.target.value);
            setBad(null);
          }}
          placeholder={EXAMPLE_CODE}
          maxLength={9}
          autoCapitalize="characters"
          autoComplete="off"
          className="font-mono"
          error={
            bad === "code"
              ? tr("join_code_unknown", undefined, "No one nearby is hosting with that code. Check it, or paste the link.")
              : undefined
          }
          data-testid="join-code"
        />
        <Button
          type="submit"
          variant="primary"
          disabled={(!text.trim() && !codeText.trim()) || !name.trim()}
          data-testid="join-app-go"
        >
          {tr("join_button", undefined, "Join")}
        </Button>
      </form>

      <section
        className="space-y-2 rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface-raised)] p-4"
        data-testid="join-nearby"
      >
        <h3 className="flex items-center gap-2 text-sm font-semibold">
          <Radar size={16} />
          {tr("join_nearby_title", undefined, "Playing nearby")}
        </h3>
        {nearby.length === 0 ? (
          <p className="text-xs text-[var(--color-muted)]">
            {tr("join_nearby_none", undefined, "No one on this network is hosting Play Together right now.")}
          </p>
        ) : (
          <ul className="space-y-2">
            {nearby.map(({ device, join: j }) => {
              const who = j.host_name || deviceName(device);
              return (
                <li key={j.ws} className="flex flex-wrap items-center gap-2 text-sm" data-testid="join-nearby-host">
                  <span className="min-w-0 flex-1 basis-[12rem]">
                    <span className="font-medium">
                      {tr("join_nearby_host", { name: who, console: j.console_name }, `${who}'s ${j.console_name}`)}
                    </span>
                    <span className="text-[var(--color-muted)]">
                      {j.game_title ? ` · ${j.game_title}` : ""}
                      {" · "}
                      {tr("join_nearby_players", { n: j.players, max: j.max }, `${j.players}/${j.max} players`)}
                    </span>
                  </span>
                  <Button
                    size="sm"
                    variant="primary"
                    disabled={!name.trim() || j.players >= j.max}
                    onClick={() => {
                      const link = linkFromSocket(j.ws, who);
                      if (link) void go(link, name);
                    }}
                    data-testid="join-nearby-go"
                  >
                    {tr("join_button", undefined, "Join")}
                  </Button>
                </li>
              );
            })}
          </ul>
        )}
      </section>
      <p className="text-xs text-[var(--color-muted)]" data-testid="join-elsewhere">
        {tr(
          "pt_elsewhere_guest_one_link",
          undefined,
          "Playing from somewhere else? Ask your friend for their invite link, then open it in your browser.",
        )}
      </p>
      {scanning && (
        <QrScanner
          onClose={() => setScanning(false)}
          onFound={(raw) => {
            setScanning(false);
            setText(raw);
            if (name.trim()) void join(raw, "", name);
          }}
        />
      )}
      {dialog}
    </div>
  );
}

/** Full screen for the joined game in the app: the window, sideways on a phone, held still. */
async function appFullscreen(on: boolean): Promise<void> {
  holdPageStill(on);
  if (on) lockLandscape();
  else unlockOrientation();
  await setWindowFullscreen(on);
}

type Detector = { detect: (src: CanvasImageSource) => Promise<Array<{ rawValue: string }>> };
type DetectorCtor = new (o: { formats: string[] }) => Detector;

/** A camera QR scanner exists here (BarcodeDetector: Chrome on Android, some desktops). Never the
 *  iPhone app: WebKit has no such API by default, and the app no longer asks for the camera, so
 *  it could not use one (the iPhone's own Camera app scans the code and opens the invite). */
export function scanAvailable(): boolean {
  return (
    !isIOS() &&
    typeof window !== "undefined" &&
    "BarcodeDetector" in window &&
    typeof navigator !== "undefined" &&
    !!navigator.mediaDevices?.getUserMedia
  );
}

/** getUserMedia's error for a refused permission (as opposed to no camera, or a busy one). */
export function cameraRefused(e: unknown): boolean {
  const name = (e as { name?: unknown } | null)?.name;
  return name === "NotAllowedError" || name === "SecurityError" || name === "PermissionDeniedError";
}

/** Opens the app's settings (the Android app), for a camera the user turned off; else null. */
function cameraSettings(): (() => void) | null {
  const b = (window as unknown as { PS5UploadCamera?: { openSettings(): void } }).PS5UploadCamera;
  if (!b) return null;
  return () => {
    try {
      b.openSettings();
    } catch {
      // Nothing to do: Try again is still there.
    }
  };
}

/** The camera, until it sees a Play Together QR code. */
function QrScanner({ onFound, onClose }: { onFound: (link: string) => void; onClose: () => void }) {
  // The latest callback, without restarting the camera when the parent re-renders.
  const onFoundRef = useRef(onFound);
  useEffect(() => {
    onFoundRef.current = onFound;
  }, [onFound]);
  const tr = useTr();
  const video = useRef<HTMLVideoElement>(null);
  // "denied": the camera permission was refused (ask again, or the app's settings).
  const [failed, setFailed] = useState<false | "denied" | "other">(false);
  const [attempt, setAttempt] = useState(0);
  // A code in view that isn't an invite: said, so the camera doesn't look stuck.
  const [wrong, setWrong] = useState(false);
  useEffect(() => {
    let stream: MediaStream | null = null;
    let timer = 0;
    let alive = true;
    void (async () => {
      try {
        stream = await navigator.mediaDevices.getUserMedia({ video: { facingMode: "environment" }, audio: false });
        if (!alive) return stream.getTracks().forEach((t) => t.stop());
        const v = video.current;
        if (!v) return;
        v.srcObject = stream;
        await v.play();
        const Ctor = (window as unknown as { BarcodeDetector: DetectorCtor }).BarcodeDetector;
        const det = new Ctor({ formats: ["qr_code"] });
        const look = async () => {
          if (!alive) return;
          try {
            const found = await det.detect(v);
            const hit = found.map((f) => f.rawValue).find((r) => !!parseJoinLink(r));
            if (hit) return onFoundRef.current(hit);
            setWrong(found.length > 0);
          } catch {
            // The next frame.
          }
          timer = window.setTimeout(() => void look(), 250);
        };
        void look();
      } catch (e) {
        if (alive) setFailed(cameraRefused(e) ? "denied" : "other");
      }
    })();
    return () => {
      alive = false;
      window.clearTimeout(timer);
      stream?.getTracks().forEach((t) => t.stop());
    };
  }, [attempt]);
  const settings = cameraSettings();
  return (
    <div className="fixed inset-0 z-[70] flex flex-col items-center justify-center gap-3 bg-black/90 p-4 text-white" data-testid="join-scanner">
      {failed === "denied" ? (
        <>
          <p className="max-w-sm text-center text-sm" data-testid="join-scan-denied">
            {tr("join_scan_denied", undefined, "PS5Upload needs the camera to scan the code. Press Try again and allow it, or turn on the camera in settings.")}
          </p>
          <div className="flex gap-2">
            <Button size="sm" onClick={() => {
                setFailed(false);
                setAttempt((n) => n + 1);
              }}>
              {tr("join_retry", undefined, "Try again")}
            </Button>
            {settings && (
              <Button size="sm" variant="secondary" onClick={settings}>
                {tr("join_scan_settings", undefined, "Open settings")}
              </Button>
            )}
          </div>
        </>
      ) : failed ? (
        <p className="max-w-sm text-center text-sm">{tr("join_scan_failed", undefined, "The camera didn't open. Paste the link instead.")}</p>
      ) : (
        <>
          <video ref={video} muted playsInline className="max-h-[70dvh] w-full max-w-md rounded-xl bg-black object-cover" />
          <p className="text-sm text-white/80" data-testid="join-scan-hint">
            {wrong
              ? tr("join_bad_link_v2", undefined, "That isn't a Play Together invite link.")
              : tr("join_scan_hint", undefined, "Point the camera at the QR code on your friend's screen.")}
          </p>
        </>
      )}
      <Button size="sm" variant="secondary" onClick={onClose}>
        {tr("join_cancel", undefined, "Cancel")}
      </Button>
    </div>
  );
}
