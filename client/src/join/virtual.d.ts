// The /join page's strings, made by vite.pagePlugins.ts `pageStrings` from src/i18n/locales: only
// the keys the page's components use, packed (`packStrings`): the keys once, then each language's
// texts in that order, 0 where it says what English says (or has nothing).
declare module "virtual:join-strings" {
  const strings: { k: string[]; l: Record<string, (string | 0)[]> };
  export default strings;
}
