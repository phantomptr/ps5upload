// Flatten CSS cascade layers (#352).
//
// Tailwind v4 wraps every rule it emits in `@layer theme/base/components/utilities`. A WebView
// without `@layer` (Safari before 15.4: macOS 11 whose system WebKit was never updated, iOS 15.3
// and older) drops each of those blocks, which leaves the app completely unstyled: the React tree
// renders, but the page is plain HTML. Flattening keeps the layer order as specificity, so the
// cascade is the same in a modern engine and the sheet still applies in an old one.
import cascadeLayers from "@csstools/postcss-cascade-layers";

export default {
  plugins: [cascadeLayers()],
};
