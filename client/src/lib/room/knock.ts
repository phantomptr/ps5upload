// Play Rooms knocks (design 9.3.6, 10.2): asking to join a room found in Rooms, Friends or Nearby.
// Over the internet a knock travels as small hub signals to the room's lobby before any address
// is exchanged; on the home network or Tailscale it rides in the hello. A password or friend pass
// is sealed to the room's public key so only the host engine can read it, over a fresh challenge
// per guest so a sealed blob can't be replayed.
//
// Seal (the byte layout the design leaves open; docs/ROOMS_DEVIATIONS.md):
//   ephemeral P-256 key pair; shared = ECDH(eph, room pk)
//   key = HKDF-SHA256(ikm shared, salt = challenge (16 B), info "ps5upload-room-knock v1") → 16 B
//   AES-128-GCM, 12-byte random nonce, additional data = the gid (UTF-8)
//   plaintext = JSON {"pw"?: "...", "pass"?: "...", "gid": "...", "c": "<challenge b64url>"}
//   blob = base64url(eph public key, uncompressed 65 B ‖ nonce 12 B ‖ ciphertext with tag)

import type { Via } from "./room";

export type KnockNo = "no" | "full" | "locked" | "password" | "locked_out" | "host_away" | "not_listed" | "old" | "name_taken";

const NOS: readonly KnockNo[] = ["no", "full", "locked", "password", "locked_out", "host_away", "not_listed", "old", "name_taken"];

/** The host's answers to a knock. */
export type KnockReply =
  | { kind: "challenge"; c: Uint8Array }
  | { kind: "wait" }
  | { kind: "ok" }
  | { kind: "no"; why: KnockNo; triesLeft: number | null; suggest: string | null };

export function b64url(b: Uint8Array): string {
  let s = "";
  for (const x of b) s += String.fromCharCode(x);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

export function fromB64url(s: string): Uint8Array<ArrayBuffer> | null {
  if (!/^[A-Za-z0-9_-]*$/.test(s)) return null;
  try {
    const bin = atob(s.replace(/-/g, "+").replace(/_/g, "/") + "===".slice((s.length + 3) % 4));
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  } catch {
    return null;
  }
}

/** A knock answer from the host (`data.knock` of a hub signal, or the engine's `knock` message). */
export function readKnockReply(data: unknown): KnockReply | null {
  if (!data || typeof data !== "object") return null;
  const o = data as Record<string, unknown>;
  const k = o.knock;
  if (k === "challenge") {
    const c = typeof o.c === "string" ? fromB64url(o.c) : null;
    return c && c.length >= 8 && c.length <= 64 ? { kind: "challenge", c } : null;
  }
  if (k === "wait") return { kind: "wait" };
  if (k === "ok") return { kind: "ok" };
  if (k === "no") {
    const why = typeof o.why === "string" && (NOS as readonly string[]).includes(o.why) ? (o.why as KnockNo) : "no";
    const left = typeof o.tries_left === "number" ? o.tries_left : null;
    const nt = o.name_taken as { suggest?: unknown } | undefined;
    const suggest = typeof o.suggest === "string" ? o.suggest : typeof nt?.suggest === "string" ? nt.suggest : null;
    return { kind: "no", why, triesLeft: left, suggest };
  }
  return null;
}

/** The first knock signal: who asks and how they found the room. */
export function knockHello(o: { via: Via; name: string; device: string; client: string; ver: string }): Record<string, unknown> {
  return { knock: { via: o.via, name: o.name, device: o.device, client: o.client, ver: o.ver } };
}

/** What a knock carries to the host engine only. */
export interface KnockSecret {
  pw?: string;
  /** A friend pass (Play again / Friends). */
  pass?: string;
}

const INFO = new TextEncoder().encode("ps5upload-room-knock v1");

/** Seals `secret` to the room key `pk` (base64url, uncompressed P-256) for `gid` over the host's
 *  challenge. Null where this browser has no WebCrypto (a plain-http page): the knock then can't
 *  carry a password, and the page says to open the secure link. */
export async function seal(pk: string, gid: string, challenge: Uint8Array, secret: KnockSecret, cryptoImpl: Crypto | undefined = globalThis.crypto): Promise<string | null> {
  const subtle = cryptoImpl?.subtle;
  const raw = fromB64url(pk);
  if (!subtle || !raw || raw.length !== 65 || raw[0] !== 4) return null;
  try {
    const roomKey = await subtle.importKey("raw", raw, { name: "ECDH", namedCurve: "P-256" }, false, []);
    const eph = (await subtle.generateKey({ name: "ECDH", namedCurve: "P-256" }, true, ["deriveBits"])) as CryptoKeyPair;
    const shared = await subtle.deriveBits({ name: "ECDH", public: roomKey }, eph.privateKey, 256);
    const hk = await subtle.importKey("raw", shared, "HKDF", false, ["deriveKey"]);
    const salt = new Uint8Array(challenge);
    const aes = await subtle.deriveKey({ name: "HKDF", hash: "SHA-256", salt, info: INFO }, hk, { name: "AES-GCM", length: 128 }, false, ["encrypt"]);
    const nonce = cryptoImpl.getRandomValues(new Uint8Array(12));
    const body = new TextEncoder().encode(JSON.stringify({ ...secret, gid, c: b64url(challenge) }));
    const ct = new Uint8Array(
      await subtle.encrypt({ name: "AES-GCM", iv: nonce, additionalData: new TextEncoder().encode(gid) }, aes, body),
    );
    const pub = new Uint8Array(await subtle.exportKey("raw", eph.publicKey));
    const out = new Uint8Array(pub.length + nonce.length + ct.length);
    out.set(pub, 0);
    out.set(nonce, pub.length);
    out.set(ct, pub.length + nonce.length);
    return b64url(out);
  } catch {
    return null;
  }
}

/** The knock's state, as every surface shows it (name → password → waiting → playing). */
export type KnockStage =
  | { kind: "idle" }
  | { kind: "sending" }
  | { kind: "waiting"; since: number }
  | { kind: "ok" }
  | { kind: "no"; why: KnockNo; triesLeft: number | null; suggest: string | null }
  | { kind: "timeout" }
  | { kind: "failed" };

/** How long a knock waits for the host's answer (the engine's ASK_WAIT). */
export const KNOCK_WAIT_MS = 90_000;

/** The next stage from a reply. */
export function knockStep(stage: KnockStage, r: KnockReply, now: number): KnockStage {
  switch (r.kind) {
    case "challenge":
      return { kind: "sending" };
    case "wait":
      return stage.kind === "waiting" ? stage : { kind: "waiting", since: now };
    case "ok":
      return { kind: "ok" };
    case "no":
      return { kind: "no", why: r.why, triesLeft: r.triesLeft, suggest: r.suggest };
  }
}

type Tr = (key: string, vars: Record<string, string | number> | undefined, fallback: string) => string;

/** A refusal in the room's words (design 8), the same on every surface. A blocked person gets
 *  the plain "said no" (never told they are blocked). */
export function knockNoText(tr: Tr, why: KnockNo, host: string, triesLeft: number | null = null): string {
  switch (why) {
    case "full":
      return tr("room_no_full", undefined, "The room is full.");
    case "locked":
      return tr("room_no_locked", undefined, "The room isn't taking players right now.");
    case "host_away":
      return tr("room_no_host_away", undefined, "Your host isn't there to answer right now.");
    case "password":
      return triesLeft != null
        ? tr("room_no_password_left", { n: triesLeft }, `Wrong password. ${triesLeft} tries left.`)
        : tr("room_no_password", undefined, "Wrong password.");
    case "locked_out":
      return tr("room_no_locked_out", undefined, "Too many tries. Try again in 10 minutes.");
    case "not_listed":
      return tr("room_no_closed", undefined, "This room is closed.");
    case "old":
      return tr("room_no_old", undefined, "Update to join (needs PS5Upload 13.0.0 or newer).");
    case "name_taken":
      return tr("room_no_name_taken", undefined, "Someone in this room already has that name.");
    default:
      return tr("room_no_said_no", { host }, `${host} said no.`);
  }
}
