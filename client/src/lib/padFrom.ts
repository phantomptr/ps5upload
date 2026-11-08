// What feeds the pad Remote Play sends, for the engine's `GET /api/ps5/rp/controller` (and MCP
// `rp_controller`): the controllers read, how each is laid out, and which reader gave it (the
// app's own reader names a controller "<name> (app)", lib/nativePad.ts), plus the inputs used
// this session. Sent with a pad only when it changed (lib/remotePlayStream.ts sendPad): nothing
// here runs by itself.

import type { PadKind } from "./padMappings";
import { IDLE_PAD, type PadState } from "./remotePlayStream";

export type PadInput = "controller" | "keys" | "touch" | "touchpad" | "controls";

export interface PadFrom {
  inputs: PadInput[];
  pads: Array<{ id: string; mapping: string; profile: PadKind; reader: "native" | "browser" }>;
}

/** True when `p` holds anything. */
export function padActive(p: PadState): boolean {
  return (
    p.buttons !== IDLE_PAD.buttons ||
    p.l2 !== 0 ||
    p.r2 !== 0 ||
    p.lx !== 0 ||
    p.ly !== 0 ||
    p.rx !== 0 ||
    p.ry !== 0 ||
    !!p.touch
  );
}

/** The controllers as the engine is told about them. */
export function describePads(pads: ReadonlyArray<Gamepad | null>, kindOf: (g: Gamepad) => PadKind): PadFrom["pads"] {
  return pads
    .filter((g): g is Gamepad => !!g?.connected)
    .slice(0, 4)
    .map((g) => ({
      id: g.id.slice(0, 96),
      mapping: g.mapping || "none",
      profile: kindOf(g),
      reader: g.id.endsWith(" (app)") ? "native" : "browser",
    }));
}

/** `{key, value}` for sendPad: the key changes only when the description does. */
export function padFromMessage(inputs: ReadonlySet<PadInput>, pads: PadFrom["pads"]): { key: string; value: PadFrom } {
  const value: PadFrom = { inputs: [...inputs].sort(), pads };
  return { key: JSON.stringify(value), value };
}
