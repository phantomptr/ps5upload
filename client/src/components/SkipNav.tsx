import { useTr } from "../state/lang";

/**
 * Skip-to-main-content link. Should be the first focusable element in
 * the DOM (mounted at the top of AppShell). Visually hidden until
 * focused (`.skip-link` in index.css), then appears top-left.
 *
 * Tab once from the URL bar → focus lands here → Enter → focus jumps
 * to `#main`.
 */
export function SkipNav() {
  const tr = useTr();
  return (
    <a href="#main" className="skip-link">
      {tr("skip_to_main", "Skip to main content")}
    </a>
  );
}
