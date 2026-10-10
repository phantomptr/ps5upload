// Flatten CSS cascade layers (#352).
//
// Tailwind v4 wraps every rule it emits in `@layer theme/base/components/utilities`. A WebView
// without `@layer` (Safari before 15.4: macOS 11 whose system WebKit was never updated, iOS 15.3
// and older) drops each of those blocks, which leaves the app completely unstyled: the React tree
// renders, but the page is plain HTML. Flattening keeps the layer order as specificity, so the
// cascade is the same in a modern engine and the sheet still applies in an old one.
//
// Tailwind nests some at-rules inside utility rules (the @supports that carries colour-mix() for
// `bg-[var(--color-x)]/10`), and the flattening drops those; they are hoisted out first.
import cascadeLayers from "@csstools/postcss-cascade-layers";
import hoistNestedAtRules from "./postcss/hoistNestedAtRules.mjs";

export default {
  plugins: [hoistNestedAtRules(), cascadeLayers()],
};
