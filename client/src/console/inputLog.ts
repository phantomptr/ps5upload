/**
 * The input log: what the PS5 browser really delivers for each controller
 * button, measured on the console itself. While enabled the page records every
 * key, pointer, wheel and history event it sees as one compact line and sends
 * them in batches to POST /api/input-log, which appends them to the helper's
 * 64 KiB ring file /data/ps5upload/webui-input.log (webui_inputlog.c).
 *
 * Off by default; the Controls screen (Task R5) turns it on while it is open
 * with setInputLogEnabled(true). Turning it off, and the page being hidden or
 * left while it is on, sends what is still queued at once (one batch, keepalive),
 * so the last presses before closing — ○ itself — are not lost.
 *
 * Privacy: a line holds the event type, key/code/keyCode, the pointer's button
 * and position, and the element's tag plus its first cw- class (static code
 * names). Never text, file names, paths, data attributes or labels; a key
 * typed into a text field is logged as "char".
 *
 * Bounds: at most one request a second, each body at most 4 KiB, a queue of at
 * most INPUT_LOG_MAX_QUEUE lines (extra events are counted, then reported as
 * one "dropped n=N" line), pointer moves sampled at most four a second.
 */
import { call, callKeepalive } from "./api";

export const INPUT_LOG_ROUTE = "/api/input-log";
export const INPUT_LOG_FLUSH_MS = 1000;
export const INPUT_LOG_MAX_BODY = 4096;
export const INPUT_LOG_MAX_QUEUE = 200;
/** The helper refuses longer lines (webui_inputlog.h WEBUI_INPUTLOG_LINE_MAX). */
export const INPUT_LOG_MAX_LINE = 160;
const MOVE_SAMPLE_MS = 250;
/** `{"events":[]}` */
const BODY_OVERHEAD = 13;
/** `,"final":true` on the last batch. */
const FINAL_OVERHEAD = 13;

/** The event fields read (a real Event has them; tests pass plain objects). */
export interface LoggedEvent {
  type: string;
  target?: unknown;
  key?: string;
  code?: string;
  keyCode?: number;
  repeat?: boolean;
  button?: number;
  clientX?: number;
  clientY?: number;
  detail?: number;
  deltaX?: number;
  deltaY?: number;
  deltaMode?: number;
}

interface ElementLike {
  tagName?: string;
  className?: unknown;
  type?: string;
  isContentEditable?: boolean;
}

/** Only the characters the helper accepts, nothing that could be read as structure. */
function token(s: string, max = 24): string {
  const t = s.replace(/[^A-Za-z0-9_.:+-]/g, "_").slice(0, max);
  return t || "-";
}

function isTextField(el: ElementLike | null): boolean {
  if (!el || typeof el.tagName !== "string") return false;
  if (el.isContentEditable || el.tagName === "TEXTAREA") return true;
  if (el.tagName !== "INPUT") return false;
  return !["button", "checkbox", "radio", "submit", "reset", "range", "color", "file"].includes(el.type ?? "text");
}

/** The element's kind: tag and its first cw- class, e.g. button.cw-focusable. */
function role(el: ElementLike | null): string {
  if (!el || typeof el.tagName !== "string") return "-";
  const tag = token(el.tagName.toLowerCase(), 12);
  const cls = typeof el.className === "string" ? el.className.split(/\s+/).find((c) => c.startsWith("cw-")) : undefined;
  return cls ? `${tag}.${token(cls, 24)}` : tag;
}

const round = (n: number | undefined) => Math.round(n ?? 0);

/** One event as a line, e.g. "1234 keydown k=Enter c=Enter kc=13 el=button.cw-focusable"; null to skip. */
export function describeEvent(e: LoggedEvent, t: number): string | null {
  const el = (e.target ?? null) as ElementLike | null;
  const head = `${Math.max(0, Math.round(t))} ${token(e.type, 24)}`;
  let line: string;
  if (e.type === "keydown" || e.type === "keyup") {
    const key = e.key ?? "";
    const hidden = isTextField(el) && key.length === 1;
    const k = hidden
      ? "char"
      : key.length !== 1
        ? token(key)
        : /^[A-Za-z0-9]$/.test(key)
          ? key
          : `U+${key.charCodeAt(0).toString(16)}`;
    const c = hidden ? "char" : token(e.code ?? "");
    const kc = hidden ? 0 : (e.keyCode ?? 0);
    line = `${head} k=${k} c=${c} kc=${kc}${e.repeat ? " r=1" : ""} el=${role(el)}`;
  } else if (e.type === "wheel") {
    line = `${head} dx=${round(e.deltaX)} dy=${round(e.deltaY)} dm=${e.deltaMode ?? 0} el=${role(el)}`;
  } else if (typeof e.clientX === "number") {
    line = `${head} b=${e.button ?? 0} x=${round(e.clientX)} y=${round(e.clientY)} d=${e.detail ?? 0} el=${role(el)}`;
  } else {
    line = head;
  }
  return line.slice(0, INPUT_LOG_MAX_LINE);
}

/** Take lines from the front of `queue` while {"events":[...]} stays within `maxBytes`. */
export function takeBatch(queue: string[], maxBytes = INPUT_LOG_MAX_BODY): string[] {
  let size = BODY_OVERHEAD;
  let n = 0;
  while (n < queue.length) {
    const add = queue[n].length + 2 + (n ? 1 : 0); // quotes, comma (lines are ASCII, nothing to escape)
    if (size + add > maxBytes) break;
    size += add;
    n++;
  }
  return queue.splice(0, n);
}

export interface InputLogDeps {
  /** `final`: the last batch before the log stops or the page goes away (sent with keepalive). */
  send: (lines: string[], final?: boolean) => Promise<unknown>;
  now: () => number;
}

export interface InputLog {
  record(e: LoggedEvent): void;
  setEnabled(on: boolean): void;
  enabled(): boolean;
  /** Send what is queued now, as one final batch (page hidden or left). */
  flushNow(): void;
}

export function createInputLog(deps: InputLogDeps): InputLog {
  let on = false;
  let t0 = 0;
  let queue: string[] = [];
  let dropped = 0;
  let lastSend = -Infinity;
  let lastMove = -Infinity;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let inFlight = false;
  /** The regular send on the wire, if any: a final batch goes after it, never alongside. */
  let inFlightDone: Promise<unknown> | null = null;

  const schedule = () => {
    if (!on || timer !== null || inFlight || (!queue.length && !dropped)) return;
    const wait = Math.max(0, lastSend + INPUT_LOG_FLUSH_MS - deps.now());
    timer = setTimeout(flush, wait);
  };

  /** The next batch, with the "dropped" note when it fits. */
  function nextBatch(final = false): string[] {
    const max = INPUT_LOG_MAX_BODY - (final ? FINAL_OVERHEAD : 0);
    const batch = takeBatch(queue, max);
    if (!queue.length && dropped) {
      const note = `${Math.round(deps.now() - t0)} dropped n=${dropped}`;
      const size = JSON.stringify({ events: batch.concat(note) }).length;
      if (size <= max) {
        batch.push(note);
        dropped = 0;
      }
    }
    return batch;
  }

  /**
   * One batch now, outside the once-a-second pacing (at most once per stop or page hide). Marked
   * final: the helper admits it even right after a regular batch (once per its 500 ms window,
   * webui_inputlog_gate_admit). Sent after a regular send still on the wire, so the two never
   * reach the helper out of order (the later one would be refused).
   */
  function sendFinal() {
    const batch = nextBatch(true);
    if (!batch.length) return;
    lastSend = deps.now();
    const go = () =>
      deps.send(batch, true).catch(() => {
        // measurement only
      });
    if (inFlightDone) void inFlightDone.then(go, go);
    else void go();
  }

  function flush() {
    timer = null;
    if (!on) return;
    const batch = nextBatch();
    if (!batch.length) return;
    inFlight = true;
    lastSend = deps.now();
    const done = deps.send(batch).catch(() => {
      // measurement only: a lost batch is not retried
    });
    inFlightDone = done;
    void done.then(() => {
      inFlight = false;
      if (inFlightDone === done) inFlightDone = null;
      schedule();
    });
  }

  return {
    record(e) {
      if (!on) return;
      const now = deps.now();
      if (e.type === "mousemove") {
        if (now - lastMove < MOVE_SAMPLE_MS) return;
        lastMove = now;
      }
      const line = describeEvent(e, now - t0);
      if (!line) return;
      if (queue.length >= INPUT_LOG_MAX_QUEUE) dropped++;
      else queue.push(line);
      schedule();
    },
    setEnabled(next) {
      if (next === on) return;
      if (!next) sendFinal();
      on = next;
      if (on) {
        t0 = deps.now();
        lastMove = -Infinity;
      } else {
        queue = [];
        dropped = 0;
        if (timer !== null) clearTimeout(timer);
        timer = null;
      }
    },
    enabled: () => on,
    flushNow() {
      if (!on) return;
      if (timer !== null) clearTimeout(timer);
      timer = null;
      sendFinal();
      schedule();
    },
  };
}

/* ---- the page's log ---------------------------------------------------- */

const WINDOW_EVENTS = [
  "keydown",
  "keyup",
  "mousedown",
  "mouseup",
  "click",
  "contextmenu",
  "wheel",
  "mousemove",
  "popstate",
  "pagehide",
  "focus",
  "blur",
  "gamepadconnected",
];
const DOCUMENT_EVENTS = ["visibilitychange", "fullscreenchange", "webkitfullscreenchange"];

const pageLog = createInputLog({
  send: (lines, final) =>
    final
      ? callKeepalive(INPUT_LOG_ROUTE, { events: lines, final: true })
      : call(INPUT_LOG_ROUTE, { events: lines }),
  now: () => (typeof performance !== "undefined" ? performance.now() : Date.now()),
});

const onEvent = (e: Event) => {
  pageLog.record(e as unknown as LoggedEvent);
  // Hidden or left while logging: send the queue now, the page may never run again.
  if (e.type === "pagehide" || (e.type === "visibilitychange" && document.visibilityState === "hidden")) pageLog.flushNow();
};
const OPTS: AddEventListenerOptions = { capture: true, passive: true };

/** Start (true) or stop (false) logging this page's input to the helper. */
export function setInputLogEnabled(on: boolean): void {
  if (on === pageLog.enabled() || typeof window === "undefined") return;
  pageLog.setEnabled(on);
  for (const t of WINDOW_EVENTS) {
    if (on) window.addEventListener(t, onEvent, OPTS);
    else window.removeEventListener(t, onEvent, OPTS);
  }
  for (const t of DOCUMENT_EVENTS) {
    if (on) document.addEventListener(t, onEvent, OPTS);
    else document.removeEventListener(t, onEvent, OPTS);
  }
}
