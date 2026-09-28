// The repo-root FAQ.md and CHANGELOG.md, as text (vite.config.ts: bundledDocs).
declare module "virtual:doc/faq" {
  const text: string;
  export default text;
}
declare module "virtual:doc/changelog" {
  const text: string;
  export default text;
}
