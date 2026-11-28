// Play Rooms knocks (design 9.3.6, 10.2): asking to join a room found in Rooms, Friends or Nearby.
// Over the internet a knock travels as small hub signals to the room's lobby before any address
// is exchanged; on the home network or Tailscale it rides in the hello. A password or friend pass
// is sealed to the room's public key so only the host engine can read it, over a fresh challenge
// per guest so a sealed blob can't be replayed.
//
// Seal (the engine's room_seal.rs, which opens it, and PS5Upload Play's src/net/seal.cpp: one
// format for the page, the apps and the engine):
//   ephemeral P-256 key pair E; shared = ECDH(E, room pk) (the 32-byte x coordinate)
//   key = HKDF-SHA256(ikm shared, salt = challenge (16 B), info = "ps5upload room knock v1") → 16 B
//   AES-128-GCM, 12-byte random nonce, no additional data
//   plaintext = JSON {"secret": "<password, or friend pass>", "gid": "...", "challenge": "<as sent>"}
//   blob = base64url(0x01 ‖ eph public key, uncompressed 65 B ‖ nonce 12 B ‖ ciphertext with tag)
// The gid and the challenge inside the sealed text bind it to this guest and this challenge.

import type { Via } from "./room";
import { ROOM_CAPS, ROOM_PROTO } from "./rooms";

export type KnockNo = "no" | "full" | "locked" | "password" | "locked_out" | "host_away" | "not_listed" | "old" | "name_taken" | "closed" | "not_playing";

const NOS: readonly KnockNo[] = ["no", "full", "locked", "password", "locked_out", "host_away", "not_listed", "old", "name_taken", "closed", "not_playing"];

/** The host's answers to a knock. */
export type KnockReply =
  | { kind: "challenge"; c: Uint8Array; pk: string | null }
  | { kind: "wait" }
  /** Let in: `token` resumes the seat on the data channel's hello (join_net/lobby.rs). */
  | { kind: "ok"; token: string | null; lobby: string | null }
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
    const pk = typeof o.pk === "string" && /^[A-Za-z0-9_-]{40,100}$/.test(o.pk) ? o.pk : null;
    return c && c.length >= 8 && c.length <= 64 ? { kind: "challenge", c, pk } : null;
  }
  if (k === "wait") return { kind: "wait" };
  if (k === "ok") {
    return { kind: "ok", token: typeof o.token === "string" && o.token ? o.token : null, lobby: typeof o.lobby === "string" && o.lobby ? o.lobby : null };
  }
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
  return { knock: { via: o.via, name: o.name, device: o.device, client: o.client, ver: o.ver, proto: ROOM_PROTO, caps: [...ROOM_CAPS] } };
}

/** What a knock carries to the host engine only. */
export interface KnockSecret {
  pw?: string;
  /** A friend pass (Play again / Friends). */
  pass?: string;
}

const INFO = new TextEncoder().encode("ps5upload room knock v1");
/** The blob's first byte: the only version room_seal.rs opens. */
const SEAL_VERSION = 1;

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
    const pub = new Uint8Array(await subtle.exportKey("raw", eph.publicKey));
    const aes = await subtle.deriveKey({ name: "HKDF", hash: "SHA-256", salt, info: INFO }, hk, { name: "AES-GCM", length: 128 }, false, ["encrypt"]);
    const nonce = cryptoImpl.getRandomValues(new Uint8Array(12));
    // One secret: the password, or the friend pass of a Play again knock (the host engine knows
    // which from the knock's `via`).
    const body = new TextEncoder().encode(JSON.stringify({ secret: secret.pw ?? secret.pass ?? "", gid, challenge: b64url(challenge) }));
    const ct = new Uint8Array(await subtle.encrypt({ name: "AES-GCM", iv: nonce }, aes, body));
    const out = new Uint8Array(1 + pub.length + nonce.length + ct.length);
    out[0] = SEAL_VERSION;
    out.set(pub, 1);
    out.set(nonce, 1 + pub.length);
    out.set(ct, 1 + pub.length + nonce.length);
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
