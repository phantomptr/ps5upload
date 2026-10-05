// Maps an install error (raw message and/or Sony code) to the FAQ section that
// explains it, so an error toast can link straight to the support matrix.
//
// The FAQ screen filters its sections by a search string (`/faq?q=…`), so an
// "anchor" here is a string the target section is guaranteed to contain: the
// error code itself, or the exact phrase the explainer is titled with. The FAQ
// sections that carry these strings are "Install routes: what works for what"
// and "My uploaded game won't launch" in FAQ.md; installErrorDoc.test.ts reads
// FAQ.md and fails if an anchor stops resolving.

export interface InstallErrorDoc {
  /** The FAQ search string that lands on the explanation. */
  query: string;
}

/** Anchor strings. Each must appear (case-insensitively) in FAQ.md. */
export const DOC_ANCHORS = {
  stagedRefused: "0x80b2116f",
  streamUnreachable: "PS5UPLOAD_PKG_HOST_IP",
  proxy: "0x80431084",
  entitlement: "missing base entitlement",
  viewProduct: "View product",
  ps4OnPs5: "isn't playable on PS5",
  e2: "E2-80B22410",
  ce: "CE-108255-1",
  wontLaunch: "won't launch",
  matrix: "support matrix",
} as const;

const RULES: Array<[RegExp, string]> = [
  [/0x80b2116f|0x80b2150f|staged route|refused this package from its own storage/i, DOC_ANCHORS.stagedRefused],
  [/0x80431084|proxy server|proxy setting/i, DOC_ANCHORS.proxy],
  [
    /0x80431064|0x80431068|0x8041013d|never reached (this computer|the engine)|cannot connect to this computer|could not reach/i,
    DOC_ANCHORS.streamUnreachable,
  ],
  [/e2-?80b22410/i, DOC_ANCHORS.e2],
  [/ce-?108255-?1/i, DOC_ANCHORS.ce],
  [/base entitlement|missing base/i, DOC_ANCHORS.entitlement],
  [/view product/i, DOC_ANCHORS.viewProduct],
  [/ps4 game.*(not|n't) playable on ps5|isn't playable on ps5/i, DOC_ANCHORS.ps4OnPs5],
];

/** The explanation for an install error, or null when there is none to link.
 *  `code` is Sony's numeric code when the caller has it. */
export function installErrorDoc(raw: string | null | undefined, code?: number): InstallErrorDoc | null {
  const hex = code && code > 0 ? ` 0x${(code >>> 0).toString(16).padStart(8, "0")}` : "";
  const text = `${raw ?? ""}${hex}`;
  if (!text.trim()) return null;
  for (const [re, query] of RULES) {
    if (re.test(text)) return { query };
  }
  return null;
}

/** The in-app route for a FAQ search. */
export function faqLink(query: string): string {
  return `/faq?q=${encodeURIComponent(query)}`;
}

/** The route an install-error toast links to: its explanation when it has
 *  one, else the install-routes matrix (the page users should read first). */
export function installErrorLink(raw: string | null | undefined, code?: number): string {
  return faqLink(installErrorDoc(raw, code)?.query ?? DOC_ANCHORS.matrix);
}
