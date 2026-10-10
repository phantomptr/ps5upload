// Hoist at-rules nested directly in a style rule out to the top level.
//
// Tailwind v4 writes some utilities with CSS nesting, the colour-with-opacity
// ones most of all:
//
//   .bg-\[var\(--color-good\)\]\/5 {
//     background-color: var(--color-good);
//     @supports (color: color-mix(in lab, red, red)) {
//       background-color: color-mix(in oklab, var(--color-good) 5%, transparent);
//     }
//   }
//
// postcss-cascade-layers (see postcss.config.mjs) drops an at-rule nested like
// that, which left only the fallback: every `bg-[var(--color-x)]/5` painted at
// full strength. Rewritten as
//
//   .sel { background-color: var(--color-good); }
//   @supports (…) { .sel { background-color: color-mix(…); } }
//
// it means the same and survives the flattening. Only at-rules whose body is
// plain declarations are moved; anything more involved is left as it was.
export default function hoistNestedAtRules() {
  return {
    postcssPlugin: "ps5upload-hoist-nested-at-rules",
    Once(root) {
      root.walkRules((rule) => {
        let after = rule;
        for (const child of [...(rule.nodes ?? [])]) {
          if (child.type !== "atrule" || !child.nodes) continue;
          if (!child.nodes.every((n) => n.type === "decl" || n.type === "comment")) continue;
          const inner = rule.clone({ nodes: [] });
          for (const n of child.nodes) inner.append(n.clone());
          const hoisted = child.clone({ nodes: [] });
          hoisted.append(inner);
          child.remove();
          after.parent.insertAfter(after, hoisted);
          after = hoisted;
        }
      });
    },
  };
}
hoistNestedAtRules.postcss = true;
