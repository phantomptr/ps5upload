// The 21 languages the app ships, each by its own name. Kept apart from state/lang so the single-
// file guest pages (join, rooms), which swap state/lang for their own, can name a room's language.

export const LANGUAGE_LIST = [
  { code: "en", label: "English" },
  { code: "zh-CN", label: "简体中文" },
  { code: "zh-TW", label: "繁體中文" },
  { code: "hi", label: "हिन्दी" },
  { code: "es", label: "Español" },
  { code: "ar", label: "العربية" },
  { code: "bn", label: "বাংলা" },
  { code: "pl", label: "Polski" },
  { code: "pt-BR", label: "Português (Brasil)" },
  { code: "ru", label: "Русский" },
  { code: "ja", label: "日本語" },
  { code: "de", label: "Deutsch" },
  { code: "fr", label: "Français" },
  { code: "ko", label: "한국어" },
  { code: "tr", label: "Türkçe" },
  { code: "vi", label: "Tiếng Việt" },
  { code: "id", label: "Bahasa Indonesia" },
  { code: "it", label: "Italiano" },
  { code: "th", label: "ไทย" },
  { code: "hu", label: "Magyar" },
  { code: "fa", label: "فارسی" },
] as const;

/** A language code as its own name ("de" → "Deutsch"); null for none, the code for an unknown. */
export function languageName(code: string | null | undefined): string | null {
  if (!code) return null;
  return LANGUAGE_LIST.find((l) => l.code === code)?.label ?? code;
}
