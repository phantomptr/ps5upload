// "Add a PS5": one scan of the home network and the Tailscale network, an invite from another
// device (a QR code where this device has a camera, or a pasted link), and a typed address as the
// fallback. What the scans and the invite find is merged by the console's identity, so a console
// found two ways is one row with both badges.

import { useCallback, useEffect, useRef, useState } from "react";
import { Camera, Home as HomeIcon, Globe, Link2, Loader2, Plus, Radar, RefreshCw } from "lucide-react";

import { Badge, Button } from "../../../components";
import { Modal } from "../../../components/Modal";
import { useTr, type Translator } from "../../../state/lang";
import { useRosterStore } from "../../../state/roster";
import { usePairingStore } from "../../../state/pairing";
import { requestRosterPull } from "../../../state/rosterShare";
import { discoverPs5, type DiscoveredHost } from "../../../api/ps5";
import { addTailnetConsole, tailnetConsoles, type FoundConsoles } from "../../../api/tailnet";
import { consoleIdentity } from "../../../api/ava1";
import { cleanConsoleAddress, hostOf } from "../../../lib/addr";
import { humanizeCaught } from "../../../lib/humanizeError";
import { addAddress, addTarget, isTailnetIp, mergeFound, parseInvite, type FoundRow, type FoundSource, type Invite } from "./model";

/** The console list is mirrored from the engine on focus (state/rosterShare.ts): ask for that now
 *  so a console the engine added shows at once. */
function pullConsoleList() {
  requestRosterPull();
}

/** Whether this device can scan a QR code itself (a camera and the browser's barcode reader). */
export function canScanQr(): boolean {
  return (
    typeof window !== "undefined" &&
    "BarcodeDetector" in window &&
    typeof navigator !== "undefined" &&
    !!navigator.mediaDevices?.getUserMedia
  );
}

/** A name for a console that has none. */
function fallbackName(addr: string): string {
  return `PS5 (${addr})`;
}

/** A Tailscale name or address (100.64.0.0/10, or a MagicDNS name): added through the engine,
 *  which finds its home address too. */
function looksTailnet(addr: string): boolean {
  return isTailnetIp(addr) || /\.ts\.net\.?$/i.test(addr);
}

function sourceLabel(tr: Translator, s: FoundSource): string {
  if (s === "home") return tr("cs_reach_home", undefined, "Home network");
  if (s === "tailscale") return tr("cs_reach_tailscale", undefined, "Tailscale");
  return tr("cs_add_src_invite", undefined, "Invite");
}

function FoundLine({
  r,
  busy,
  onAdd,
  onOpen,
}: {
  r: FoundRow;
  busy: boolean;
  onAdd: () => void;
  onOpen: (id: string) => void;
}) {
  const tr = useTr();
  const likely = r.confidence >= 50;
  return (
    <li className="flex flex-wrap items-center gap-x-3 gap-y-2 py-2.5" data-testid="consoles-found-row" data-key={r.key ?? ""}>
      <div className="min-w-0 flex-1 basis-48">
        <div className="flex flex-wrap items-center gap-1.5">
          <span className="truncate text-sm font-semibold">{r.name || addAddress(r)}</span>
          {r.sources.map((s) => (
            <Badge key={s} tone={s === "invite" ? "accent" : "neutral"} icon={s === "home" ? HomeIcon : s === "tailscale" ? Globe : Link2}>
              {sourceLabel(tr, s)}
            </Badge>
          ))}
          {!likely ? <Badge tone="warn">{tr("connection_discover_tier_mid", undefined, "possibly PS5")}</Badge> : null}
        </div>
        <div className="mt-0.5 flex flex-wrap gap-x-3 font-mono text-xs text-[var(--color-muted)]">
          {r.home ? <span>{r.home}</span> : null}
          {r.tailnet ? <span>{r.tailnet}</span> : null}
        </div>
        <div className="text-xs text-[var(--color-muted)]">
          {r.helperRunning
            ? tr("cs_add_helper_runs", undefined, "PS5Upload's helper answers")
            : tr("cs_add_helper_off", undefined, "Its helper isn't running yet: you can send it once it's added")}
        </div>
      </div>
      {r.listedId ? (
        <Button size="sm" variant="secondary" onClick={() => onOpen(r.listedId!)} data-testid="consoles-found-open">
          {tr("cs_add_open", undefined, "On your list · Open")}
        </Button>
      ) : (
        <Button
          size="sm"
          variant="primary"
          leftIcon={<Plus size={12} />}
          loading={busy}
          disabled={busy}
          onClick={onAdd}
          data-testid="consoles-found-add"
        >
          {tr("cs_add_add", undefined, "Add")}
        </Button>
      )}
    </li>
  );
}

/** Reads QR codes from the camera until one is an invite. */
function QrScanner({ onInvite, onClose }: { onInvite: (inv: Invite) => void; onClose: () => void }) {
  const tr = useTr();
  const video = useRef<HTMLVideoElement>(null);
  const [err, setErr] = useState<string | null>(null);
  // A code in view that isn't an invite: said, so the camera doesn't look stuck.
  const [wrong, setWrong] = useState(false);
  useEffect(() => {
    let stream: MediaStream | null = null;
    let timer = 0;
    let live = true;
    void (async () => {
      try {
        stream = await navigator.mediaDevices.getUserMedia({ video: { facingMode: "environment" } });
        if (!live || !video.current) return;
        video.current.srcObject = stream;
        await video.current.play();
        const Detector = (window as unknown as { BarcodeDetector: new (o: { formats: string[] }) => { detect: (v: HTMLVideoElement) => Promise<{ rawValue: string }[]> } }).BarcodeDetector;
        const det = new Detector({ formats: ["qr_code"] });
        const tick = async () => {
          if (!live || !video.current) return;
          try {
            const codes = await det.detect(video.current);
            for (const code of codes) {
              const inv = parseInvite(code.rawValue);
              if (inv) {
                onInvite(inv);
                return;
              }
            }
            setWrong(codes.length > 0);
          } catch {
            // A frame it could not read: the next one.
          }
          timer = window.setTimeout(() => void tick(), 300);
        };
        void tick();
      } catch (e) {
        setErr(humanizeCaught(e));
      }
    })();
    return () => {
      live = false;
      window.clearTimeout(timer);
      stream?.getTracks().forEach((t) => t.stop());
    };
  }, [onInvite]);
  return (
    <div className="flex flex-col gap-2" data-testid="consoles-qr-scanner">
      <video ref={video} className="aspect-square w-full max-w-xs rounded-[var(--radius-field)] bg-black object-cover" muted playsInline />
      {err ? (
        <p role="alert" className="text-xs text-[var(--color-bad)]">
          {tr("cs_add_qr_failed", { err }, "The camera didn't start ({err}). Paste the link instead.")}
        </p>
      ) : (
        <p className="text-xs text-[var(--color-muted)]" data-testid="consoles-qr-hint">
          {wrong
            ? tr("cs_add_invite_bad", undefined, "That isn't an invite link. It starts with ps5upload://console.")
            : tr("cs_add_qr_point", undefined, "Point the camera at the invite code.")}
        </p>
      )}
      <div>
        <Button size="sm" variant="ghost" onClick={onClose}>
          {tr("cancel", undefined, "Cancel")}
        </Button>
      </div>
    </div>
  );
}

export interface ScanState {
  scanning: boolean;
  lan: DiscoveredHost[];
  lanError: string | null;
  lanDone: boolean;
  tailnet: FoundConsoles | null;
  tailnetError: string | null;
  lanKeys: Record<string, string>;
}

/** The sheet's body, from its state (render-tested on its own). */
export function AddSheetView({
  scan,
  invite,
  rows,
  busy,
  typed,
  pasted,
  scanningQr,
  qrOk,
  error,
  firstRun,
  onScan,
  onAdd,
  onOpen,
  onTyped,
  onAddTyped,
  onPasted,
  onScanQr,
  onQrInvite,
  onQrClose,
}: {
  scan: ScanState;
  invite: Invite | null;
  rows: FoundRow[];
  busy: string | null;
  typed: string;
  pasted: string;
  scanningQr: boolean;
  qrOk: boolean;
  error: string | null;
  firstRun: boolean;
  onScan: () => void;
  onAdd: (r: FoundRow) => void;
  onOpen: (id: string) => void;
  onTyped: (v: string) => void;
  onAddTyped: () => void;
  onPasted: (v: string) => void;
  onScanQr: () => void;
  onQrInvite: (inv: Invite) => void;
  onQrClose: () => void;
}) {
  const tr = useTr();
  const pastedBad = pasted.trim() !== "" && !parseInvite(pasted);
  const notes: string[] = [];
  if (scan.lanDone && scan.lanError) notes.push(tr("cs_add_home_unavailable", undefined, "The home network can't be scanned from here: the desktop and Android apps can. Type the address below."));
  else if (scan.lanDone && scan.lan.length === 0) notes.push(tr("cs_add_home_none", undefined, "Nothing found on the home network. Is the PS5 on, on this network?"));
  if (scan.tailnet?.backend === "none")
    notes.push(tr("cs_add_ts_off", undefined, "This device isn't on Tailscale (Settings › Tailscale), so only the home network was scanned."));
  else if (scan.tailnet && !scan.tailnet.can_list)
    notes.push(tr("cs_add_ts_no_list", undefined, "Tailscale's device list can't be read here: type the PS5's Tailscale address below."));
  return (
    <div className="flex flex-col gap-5 p-4 text-sm" data-testid="consoles-add">
      {firstRun ? (
        <p className="text-[var(--color-muted)]">
          {tr(
            "cs_add_first_run",
            undefined,
            "Let's find your PS5. Turn it on and run your jailbreak's loader, keep it on the same network as this device, then scan.",
          )}
        </p>
      ) : null}

      <section aria-labelledby="add-scan-title" className="flex flex-col gap-2">
        <div className="flex flex-wrap items-center gap-2">
          <h3 id="add-scan-title" className="flex-1 text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
            {tr("cs_add_scan_title", undefined, "Scan")}
          </h3>
          <Button
            size="sm"
            variant="secondary"
            leftIcon={scan.scanning ? <Loader2 size={12} className="animate-spin" /> : rows.length ? <RefreshCw size={12} /> : <Radar size={12} />}
            disabled={scan.scanning}
            onClick={onScan}
            data-testid="consoles-scan"
          >
            {scan.scanning
              ? tr("cs_add_scanning", undefined, "Looking…")
              : scan.lanDone
                ? tr("connection_discover_rescan", undefined, "Scan again")
                : tr("cs_add_scan", undefined, "Scan")}
          </Button>
        </div>
        <p className="text-xs text-[var(--color-muted)]">
          {tr("cs_add_scan_hint", undefined, "Looks on your home network first, then on your Tailscale network.")}
        </p>
        {rows.length > 0 ? (
          <ul className="divide-y divide-[var(--color-border)] rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface)] px-3" data-testid="consoles-found">
            {rows.map((r) => (
              <FoundLine key={r.id} r={r} busy={busy === r.id} onAdd={() => onAdd(r)} onOpen={onOpen} />
            ))}
          </ul>
        ) : scan.scanning ? (
          <p className="flex items-center gap-2 text-xs text-[var(--color-muted)]">
            <Loader2 size={12} className="animate-spin" aria-hidden />
            {tr("cs_add_scanning_long", undefined, "Looking on your home network and Tailscale…")}
          </p>
        ) : null}
        {notes.map((n) => (
          <p key={n} className="text-xs text-[var(--color-muted)]">
            {n}
          </p>
        ))}
      </section>

      <section aria-labelledby="add-invite-title" className="flex flex-col gap-2">
        <h3 id="add-invite-title" className="text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
          {tr("cs_add_invite_title", undefined, "An invite")}
        </h3>
        <p className="text-xs text-[var(--color-muted)]">
          {tr(
            "cs_add_invite_hint",
            undefined,
            "On a device that already uses the PS5, open Connection and press Invite a device on the PS5's card. Scan its code, or paste its link here. The PS5 then asks before it lets this device in.",
          )}
        </p>
        {invite ? (
          <p className="text-xs text-[var(--color-good)]" data-testid="consoles-invite-read">
            {tr("cs_add_invite_read", { name: invite.name || addAddress(invite) }, "Invite for {name} read: it is in the list above.")}
          </p>
        ) : null}
        {scanningQr ? (
          <QrScanner onInvite={onQrInvite} onClose={onQrClose} />
        ) : (
          <div className="flex flex-wrap items-center gap-2">
            <input
              className="input min-w-0 flex-1 basis-56 font-mono text-xs max-md:min-h-11"
              value={pasted}
              onChange={(e) => onPasted(e.target.value)}
              placeholder={tr("cs_add_paste_placeholder", undefined, "ps5upload://console?…")}
              aria-label={tr("cs_add_paste", undefined, "Paste an invite link")}
              autoCapitalize="off"
              autoCorrect="off"
              spellCheck={false}
              data-testid="consoles-invite-paste"
            />
            {qrOk ? (
              <Button size="sm" variant="secondary" leftIcon={<Camera size={12} />} onClick={onScanQr} data-testid="consoles-scan-qr">
                {tr("cs_add_scan_qr", undefined, "Scan a code")}
              </Button>
            ) : null}
          </div>
        )}
        {pastedBad ? (
          <p role="alert" className="text-xs text-[var(--color-bad)]">
            {tr("cs_add_invite_bad", undefined, "That isn't an invite link. It starts with ps5upload://console.")}
          </p>
        ) : null}
      </section>

      <section aria-labelledby="add-type-title" className="flex flex-col gap-2">
        <h3 id="add-type-title" className="text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
          {tr("cs_add_type_title", undefined, "Or type its address")}
        </h3>
        <form
          className="flex flex-wrap items-center gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            onAddTyped();
          }}
        >
          <input
            className="input min-w-0 flex-1 basis-56 font-mono max-md:min-h-11"
            value={typed}
            onChange={(e) => onTyped(e.target.value)}
            placeholder={tr("cs_add_type_placeholder", undefined, "192.168.1.50, or 100.x.y.z on Tailscale")}
            aria-label={tr("cs_add_type_title", undefined, "Or type its address")}
            inputMode="url"
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
            data-testid="consoles-add-typed-input"
          />
          <Button
            type="submit"
            size="sm"
            variant="secondary"
            leftIcon={<Plus size={12} />}
            disabled={!typed.trim() || busy !== null}
            loading={busy === "typed"}
            data-testid="consoles-add-typed"
          >
            {tr("cs_add_add", undefined, "Add")}
          </Button>
        </form>
        <p className="text-xs text-[var(--color-muted)]">
          {tr(
            "connection_step1_hint",
            undefined,
            "Find this in the PS5's network settings, or on your router's device list. A wired Ethernet connection is strongly recommended over Wi-Fi.",
          )}
        </p>
      </section>

      {error ? (
        <p role="alert" className="text-xs text-[var(--color-bad)]">
          {error}
        </p>
      ) : null}
    </div>
  );
}

const EMPTY_SCAN: ScanState = {
  scanning: false,
  lan: [],
  lanError: null,
  lanDone: false,
  tailnet: null,
  tailnetError: null,
  lanKeys: {},
};

export function AddConsoleSheet({
  open,
  onClose,
  initialInvite,
  firstRun,
  onAdded,
}: {
  open: boolean;
  onClose: () => void;
  initialInvite: Invite | null;
  firstRun: boolean;
  /** A console went on the list (its roster id): the page shows its card. */
  onAdded: (id: string, fromInvite: boolean) => void;
}) {
  const tr = useTr();
  const profiles = useRosterStore((s) => s.profiles);
  const [scan, setScan] = useState<ScanState>(EMPTY_SCAN);
  const [invite, setInvite] = useState<Invite | null>(initialInvite);
  const [busy, setBusy] = useState<string | null>(null);
  const [typed, setTyped] = useState("");
  const [pasted, setPasted] = useState("");
  const [scanningQr, setScanningQr] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** A console the engine added, selected once the list mirror brings it in. */
  const [pending, setPending] = useState<{ id: string; invite: boolean } | null>(null);

  useEffect(() => {
    if (initialInvite) setInvite(initialInvite);
  }, [initialInvite]);

  const runScan = useCallback(async () => {
    setScan((s) => ({ ...s, scanning: true, lanError: null, tailnetError: null }));
    const lanP = discoverPs5()
      .then((r) => ({ lan: r.candidates, err: r.error ?? null }))
      .catch((e) => ({ lan: [] as DiscoveredHost[], err: humanizeCaught(e) }));
    const tnP = tailnetConsoles(true).catch(() => null);
    const lan = await lanP;
    // The console's identity at each home address its helper answers at: what merges it with the
    // same console found on Tailscale or named by an invite.
    const keys: Record<string, string> = {};
    await Promise.all(
      lan.lan
        .filter((h) => h.payload_port_open)
        .map(async (h) => {
          const id = await consoleIdentity(h.ip);
          if (id) keys[h.ip] = id.key;
        }),
    );
    setScan((s) => ({ ...s, lan: lan.lan, lanError: lan.err, lanDone: true, lanKeys: keys }));
    const tn = await tnP;
    setScan((s) => ({ ...s, scanning: false, tailnet: tn }));
  }, []);

  useEffect(() => {
    if (open) void runScan();
    else {
      // Closed: nothing carries over to the next Add (an invite already used, say).
      setScan(EMPTY_SCAN);
      setScanningQr(false);
      setError(null);
      setInvite(initialInvite);
      setPasted("");
      setTyped("");
    }
  }, [open, runScan, initialInvite]);

  // The engine added it; the list mirror normally brings it in within a second.
  useEffect(() => {
    if (!pending) return;
    const t = window.setTimeout(() => {
      setPending(null);
      setError(
        tr(
          "cs_add_slow",
          undefined,
          "The PS5 was added, but this list hasn't shown it yet. Close this sheet: its card appears on Consoles in a moment.",
        ),
      );
    }, 10_000);
    return () => window.clearTimeout(t);
  }, [pending, tr]);

  useEffect(() => {
    const inv = parseInvite(pasted);
    if (inv) setInvite(inv);
  }, [pasted]);

  useEffect(() => {
    if (pending && profiles.some((p) => p.id === pending.id)) {
      onAdded(pending.id, pending.invite);
      setPending(null);
    }
  }, [pending, profiles, onAdded]);

  const rows = mergeFound(scan.lan, scan.tailnet?.consoles ?? [], invite, scan.lanKeys, profiles);

  const addAt = async (addr: string, name: string, busyKey: string, fromInvite: boolean, inviteKey: string | null = null) => {
    const value = cleanConsoleAddress(addr);
    if (!value) return;
    setBusy(busyKey);
    setError(null);
    try {
      // An invite names the console's key: the address must answer as that console, or the link
      // was changed (or the address now belongs to another PS5). One that does not answer yet (its
      // helper is off) can't be checked, and is added as named.
      if (fromInvite && inviteKey) {
        const id = await consoleIdentity(value);
        if (id && id.key.toLowerCase() !== inviteKey.toLowerCase()) {
          setError(
            tr(
              "cs_add_invite_other_ps5",
              undefined,
              "A different PS5 answers at the address in this invite. Ask for a new invite on a device that uses the PS5, or add it with Scan.",
            ),
          );
          return;
        }
      }
      const roster = useRosterStore.getState();
      let pairAt = value;
      const existing = roster.profiles.find((p) => hostOf(p.host) === hostOf(value));
      if (existing) {
        onAdded(existing.id, fromInvite);
      } else if (looksTailnet(value)) {
        // The engine keeps one entry per console, at its home address when it knows one.
        const r = await addTailnetConsole(value, name);
        pairAt = r.console.host || value;
        setPending({ id: r.console.id, invite: fromInvite });
        pullConsoleList();
      } else {
        const id = roster.add({ name: name || fallbackName(value), host: value });
        onAdded(id, fromInvite);
      }
      if (fromInvite) {
        // The console asks before it lets this device in: the pairing dialog waits for the Allow,
        // at the address the list keeps for it.
        void usePairingStore.getState().openFor(pairAt);
      }
    } catch (e) {
      setError(
        tr(
          "cs_add_failed",
          { err: humanizeCaught(e) },
          "Couldn't add this PS5 ({err}). Check that it is on and that the address is right, then press Add again.",
        ),
      );
    } finally {
      setBusy(null);
    }
  };

  return (
    <Modal open={open} onClose={onClose} title={tr("cs_add_title", undefined, "Add a PS5")} size="lg">
      <AddSheetView
        scan={scan}
        invite={invite}
        rows={rows}
        busy={busy}
        typed={typed}
        pasted={pasted}
        scanningQr={scanningQr}
        qrOk={canScanQr()}
        error={error}
        firstRun={firstRun}
        onScan={() => void runScan()}
        onAdd={(r) => {
          const invited = r.sources.includes("invite");
          const onTs = !!scan.tailnet && scan.tailnet.backend !== "none";
          void addAt(addTarget(r, onTs), r.name, r.id, invited, invited ? r.key : null);
        }}
        onOpen={(id) => onAdded(id, false)}
        onTyped={setTyped}
        onAddTyped={() => {
          const v = typed.trim();
          if (!v) return;
          setTyped("");
          void addAt(v, "", "typed", false);
        }}
        onPasted={setPasted}
        onScanQr={() => setScanningQr(true)}
        onQrInvite={(inv) => {
          setInvite(inv);
          setScanningQr(false);
        }}
        onQrClose={() => setScanningQr(false)}
      />
    </Modal>
  );
}
