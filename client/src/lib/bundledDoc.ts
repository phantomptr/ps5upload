// FAQ.md and CHANGELOG.md for their screens. The desktop app reads the copy it was built with;
// the browser build (no native commands) loads the same file bundled into the web app, fetched
// only when the screen opens.

import { invoke } from "./invokeLogged";
import { isTauriEnv } from "./tauriEnv";

export async function loadBundledDoc(doc: "faq" | "changelog"): Promise<string> {
  if (isTauriEnv()) return invoke<string>(doc === "faq" ? "faq_load" : "changelog_load");
  const m =
    doc === "faq" ? await import("virtual:doc/faq") : await import("virtual:doc/changelog");
  return m.default;
}
