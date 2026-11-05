/**
 * Full screen for the console page: the PS5 browser's own bars take part of a
 * 1540×700 viewport, so on the PS5 browser the page asks for full screen on
 * the first user activation (a click or a key: browsers only grant it inside
 * one) while the Settings switch is on (the default). A LAN viewer (phone,
 * desktop) is never put in full screen by itself; the switch still works there.
 *
 * The standard API first, then WebKit's prefixed one. Where neither exists, the
 * browser disables it, or it refuses a request (a fullscreenerror, a rejected
 * promise), full screen counts as unsupported for the rest of the session:
 * nothing more is asked, Settings hides the switch and the layout simply
 * scales to the viewport.
 */

type Fn = () => void;

/** The parts of `document` this module uses (tests pass a stand-in). */
export interface FullscreenDoc {
  documentElement: Element & {
    requestFullscreen?: () => Promise<void>;
    webkitRequestFullscreen?: () => void;
  };
  fullscreenEnabled?: boolean;
  webkitFullscreenEnabled?: boolean;
  fullscreenElement?: Element | null;
  webkitFullscreenElement?: Element | null;
  exitFullscreen?: () => Promise<void>;
  webkitExitFullscreen?: () => void;
  addEventListener?: (type: string, fn: Fn) => void;
  removeEventListener?: (type: string, fn: Fn) => void;
}

interface Listener {
  addEventListener(type: string, fn: (e: KeyboardEvent | MouseEvent) => void, capture?: boolean): void;
  removeEventListener(type: string, fn: (e: KeyboardEvent | MouseEvent) => void, capture?: boolean): void;
}

const STORAGE_KEY = "ps5upload.console.fullscreen";
/** Activations that may carry the request. */
const ACTIVATIONS = ["click", "keydown"];
const CHANGE_EVENTS = ["fullscreenchange", "webkitfullscreenchange"];
const ERROR_EVENTS = ["fullscreenerror", "webkitfullscreenerror"];
/** How long a webkit request may stay unanswered before it counts as not taken. */
const WEBKIT_ANSWER_MS = 1500;
/** Requests that neither entered nor were refused (silently ignored) before giving up. */
const MAX_TRIES = 3;

/** The browser refused a request this session: full screen is off the table. */
let refused = false;

/** Tests only: forget a refusal. */
export function _resetFullscreenForTests(): void {
  refused = false;
}

function currentDoc(): FullscreenDoc {
  return document as unknown as FullscreenDoc;
}

/**
 * The console's own browser: its user agent names a PlayStation, however the firmware spells the
 * rest ("PlayStation; PlayStation 5/13.60", "PlayStation 5 13.60"). The one test the page uses for
 * "this is the console itself": ○ leaves the page, Upload is not offered, full screen by itself.
 */
export function isPs5Browser(ua: string = typeof navigator !== "undefined" ? navigator.userAgent : ""): boolean {
  return /PlayStation/i.test(ua);
}

/** Which API the browser has: the standard one, WebKit's prefixed one, or none. */
export function fullscreenApi(doc: FullscreenDoc = currentDoc()): "standard" | "webkit" | null {
  const el = doc.documentElement;
  if (el && typeof el.requestFullscreen === "function") return "standard";
  if (el && typeof el.webkitRequestFullscreen === "function") return "webkit";
  return null;
}

/** Whether the page can go full screen: an API, not disabled, and not refused this session. */
export function fullscreenSupported(doc: FullscreenDoc = currentDoc()): boolean {
  if (refused) return false;
  const api = fullscreenApi(doc);
  if (api === "standard") return doc.fullscreenEnabled !== false;
  if (api === "webkit") return doc.webkitFullscreenEnabled !== false;
  return false;
}

export function isFullscreen(doc: FullscreenDoc = currentDoc()): boolean {
  return !!(doc.fullscreenElement || doc.webkitFullscreenElement);
}

const watchers = new Set<Fn>();
function notify(): void {
  for (const fn of Array.from(watchers)) fn();
}
function markRefused(): void {
  if (refused) return;
  refused = true;
  notify();
}

/**
 * Ask for full screen. Resolves true once the page is full screen. A refusal
 * (rejected promise, error event) resolves false and marks full screen
 * unsupported for the session. A webkit request the browser answers with
 * neither event is not counted as success: after WEBKIT_ANSWER_MS the real
 * state decides.
 */
export function enterFullscreen(doc: FullscreenDoc = currentDoc()): Promise<boolean> {
  if (!fullscreenSupported(doc)) return Promise.resolve(false);
  if (fullscreenApi(doc) === "standard") {
    try {
      return Promise.resolve(doc.documentElement.requestFullscreen!()).then(
        () => true,
        () => {
          markRefused();
          return false;
        },
      );
    } catch {
      markRefused();
      return Promise.resolve(false);
    }
  }
  return new Promise<boolean>((resolve) => {
    let done = false;
    const finish = (ok: boolean) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      doc.removeEventListener?.("webkitfullscreenchange", onChange);
      doc.removeEventListener?.("webkitfullscreenerror", onError);
      resolve(ok);
    };
    const onChange = () => finish(isFullscreen(doc));
    const onError = () => {
      markRefused();
      finish(false);
    };
    const timer = setTimeout(() => finish(isFullscreen(doc)), WEBKIT_ANSWER_MS);
    doc.addEventListener?.("webkitfullscreenchange", onChange);
    doc.addEventListener?.("webkitfullscreenerror", onError);
    try {
      doc.documentElement.webkitRequestFullscreen!();
    } catch {
      onError();
    }
  });
}

export function exitFullscreen(doc: FullscreenDoc = currentDoc()): void {
  try {
    if (doc.fullscreenElement && typeof doc.exitFullscreen === "function") {
      doc.exitFullscreen().catch(() => {});
    } else if (doc.webkitFullscreenElement && typeof doc.webkitExitFullscreen === "function") {
      doc.webkitExitFullscreen();
    }
  } catch {
    // already left
  }
}

/** The Settings switch's stored wish: on unless the viewer turned it off (also when storage fails). */
export function fullscreenPref(): boolean {
  try {
    return localStorage.getItem(STORAGE_KEY) !== "0";
  } catch {
    return true;
  }
}

/**
 * Flip the switch. Called from the switch's own click, a user activation, so
 * turning it on goes full screen at once; turning it off leaves full screen.
 */
export function setFullscreenPref(on: boolean, doc: FullscreenDoc = currentDoc()): void {
  try {
    localStorage.setItem(STORAGE_KEY, on ? "1" : "0");
  } catch {
    // remembered for this page only
  }
  if (on) void enterFullscreen(doc);
  else exitFullscreen(doc);
}

/** The touchpad button: in or out of full screen (from a key, so inside a user activation). */
export function toggleFullscreen(doc: FullscreenDoc = currentDoc()): void {
  if (isFullscreen(doc)) exitFullscreen(doc);
  else void enterFullscreen(doc);
}

export interface FullscreenState {
  /** The switch can be shown. */
  supported: boolean;
  /** The page is full screen now. */
  on: boolean;
}

export function fullscreenState(doc: FullscreenDoc = currentDoc()): FullscreenState {
  return { supported: fullscreenSupported(doc), on: isFullscreen(doc) };
}

/**
 * Call `fn` with the new state whenever full screen is entered, left or
 * refused (Settings keeps its switch true to the page). Returns the unsubscribe.
 */
export function watchFullscreen(fn: (s: FullscreenState) => void, doc: FullscreenDoc = currentDoc()): () => void {
  const run = () => fn(fullscreenState(doc));
  const onError = () => markRefused(); // markRefused notifies every watcher, this one included
  watchers.add(run);
  for (const t of CHANGE_EVENTS) doc.addEventListener?.(t, run);
  for (const t of ERROR_EVENTS) doc.addEventListener?.(t, onError);
  return () => {
    watchers.delete(run);
    for (const t of CHANGE_EVENTS) doc.removeEventListener?.(t, run);
    for (const t of ERROR_EVENTS) doc.removeEventListener?.(t, onError);
  };
}

/**
 * On the PS5 browser, go full screen on the first user activation (Escape
 * is not one) while the switch is on. Once full screen was entered or refused,
 * or after MAX_TRIES silently ignored requests, the listeners go. Leaving full
 * screen later (the browser's own way) is respected: no re-entry.
 */
export function installFullscreen(
  win: Listener = window,
  doc: FullscreenDoc = currentDoc(),
  ua: string = typeof navigator !== "undefined" ? navigator.userAgent : "",
): () => void {
  if (!isPs5Browser(ua)) return () => {};
  let tries = 0;
  let pending = false;
  let removed = false;
  const onError = () => markRefused();
  const remove = () => {
    if (removed) return;
    removed = true;
    for (const t of ACTIVATIONS) win.removeEventListener(t, onActivation, true);
  };
  function onActivation(e: KeyboardEvent | MouseEvent) {
    if (removed || pending) return;
    if (e && (e as KeyboardEvent).key === "Escape") return; // a key event; a click has no key
    if (!fullscreenSupported(doc) || isFullscreen(doc)) {
      remove();
      return;
    }
    if (!fullscreenPref()) return;
    tries++;
    pending = true;
    void enterFullscreen(doc).then((ok) => {
      pending = false;
      if (ok || refused || tries >= MAX_TRIES) remove();
    });
  }
  for (const t of ERROR_EVENTS) doc.addEventListener?.(t, onError);
  for (const t of ACTIVATIONS) win.addEventListener(t, onActivation, true);
  return () => {
    remove();
    for (const t of ERROR_EVENTS) doc.removeEventListener?.(t, onError);
  };
}
