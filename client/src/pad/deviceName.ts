// What a phone calls itself on the computer's list, from its browser. A short word, not a name in
// English: the computer's own app says it in its language (screens/Play/PhonePads.tsx).

export type DeviceWord = "iphone" | "ipad" | "android" | "phone";

/** iPadOS asks for desktop sites, so its Safari says "Macintosh": a Mac has no touch screen. */
export function deviceWord(ua: string, maxTouchPoints = 0): DeviceWord {
  if (/iPad/.test(ua) || (/Macintosh/.test(ua) && maxTouchPoints > 1)) return "ipad";
  if (/iPhone|iPod/.test(ua)) return "iphone";
  if (/Android/.test(ua)) return "android";
  return "phone";
}
