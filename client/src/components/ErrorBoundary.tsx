import { Component, type ErrorInfo, type ReactNode } from "react";
import { log } from "../state/logs";
import { useLangStore } from "../state/lang";
import { t as translate } from "../i18n";
import { isTauriEnv } from "../lib/tauriEnv";

interface Props {
  children: ReactNode;
}
interface State {
  err: Error | null;
  componentStack: string;
}

/**
 * Top-level React error boundary. A render-time exception in any
 * descendant lands here instead of unmounting the whole tree and
 * leaving the user with a white window. Mirrors the captured exception
 * to the in-app Log store so the user can grab it from the Logs tab
 * for a bug report.
 *
 * Recovery is best-effort: clicking "Try again" resets the boundary
 * state, which re-renders the children. If the error is deterministic
 * the boundary will catch it again on the next render — at that point
 * the user's only option is "Reload window," which forces Tauri's
 * webview to refresh the JS bundle.
 */
export class RootErrorBoundary extends Component<Props, State> {
  state: State = { err: null, componentStack: "" };
  /** Unsubscribe handle for the language-store subscription. */
  private unsubLang: (() => void) | null = null;

  static getDerivedStateFromError(err: Error): State {
    return { err, componentStack: "" };
  }

  componentDidMount() {
    // This is a class component, so `useTr()` (a hook) is off-limits.
    // Subscribe to the language store directly and force a re-render
    // when `lang` changes so the crash screen still respects a
    // language switch. The subscription only matters while the error
    // UI is on screen — but it's cheap to keep for the boundary's
    // whole lifetime and avoids wiring it up inside componentDidCatch.
    this.unsubLang = useLangStore.subscribe((s, prev) => {
      if (s.lang !== prev.lang) this.forceUpdate();
    });
  }

  componentWillUnmount() {
    this.unsubLang?.();
    this.unsubLang = null;
  }

  /** Non-hook translator for this class component. Reads the current
   *  language straight off the store and falls back to the English
   *  string when the key is absent (mirrors `useTr()`'s 2-arg form).
   *  The locale dict may still be lazy-loading when a crash happens;
   *  `translate()` returns the key itself in that case, so we fall
   *  back to English — the right behaviour for an error screen. */
  private tr = (key: string, fallback: string): string => {
    const result = translate(useLangStore.getState().lang, key);
    return result === key ? fallback : result;
  };

  componentDidCatch(err: Error, info: ErrorInfo) {
    // Send to the in-app log store so the Logs tab shows it. We use
    // log.error rather than console.error: the latter is already
    // captured by installConsoleCapture (main.tsx), but landing it
    // through the structured `log.*` API gives the entry a category
    // ("react") that filters cleanly in the UI.
    log.error("react", err.message, {
      stack: err.stack ?? "",
      componentStack: info.componentStack ?? "",
    });
    // Auto-capture a crash report for a hard render crash. force=true: these
    // are rare and always worth a report (bypass the debounce). Fire-and-
    // forget; the reporter never throws. Dynamic import keeps it off the
    // hot path and avoids a cycle through the logs/notifications stores.
    void import("../lib/crashReporter").then((m) =>
      m.captureCrashReport(`frontend-error: ${err.message}`, { force: true }),
    );
    this.setState({ componentStack: info.componentStack ?? "" });
  }

  render() {
    if (this.state.err) {
      const { err, componentStack } = this.state;
      return (
        <div
          role="alert"
          aria-live="assertive"
          className="flex min-h-screen items-center justify-center bg-[var(--color-bg)] p-6"
        >
          <div className="w-full max-w-xl rounded-xl border border-[var(--color-bad)] bg-[var(--color-bad-soft)] p-6">
            <div className="text-base font-semibold text-[var(--color-bad)]">
              {this.tr(
                "errorboundary_something_wrong",
                "Something went wrong rendering this screen.",
              )}
            </div>
            <div className="mt-1 break-words text-sm text-[var(--color-text)]">
              {err.name}: {err.message}
            </div>
            <details className="mt-3 text-xs text-[var(--color-muted)]">
              <summary className="cursor-pointer select-none">
                {this.tr("errorboundary_show_stack", "Show stack trace")}
              </summary>
              <pre className="mt-2 max-h-64 overflow-auto whitespace-pre-wrap rounded bg-[var(--color-surface)] p-3 font-mono text-xs leading-tight">
                {err.stack ?? "(no stack)"}
                {componentStack ? `\n\n${componentStack}` : ""}
              </pre>
            </details>
            <div className="mt-4 flex gap-2">
              <button
                type="button"
                onClick={() =>
                  this.setState({ err: null, componentStack: "" })
                }
                className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-1.5 text-sm font-medium hover:bg-[var(--color-surface-hover)]"
              >
                {this.tr("errorboundary_try_again", "Try again")}
              </button>
              <button
                type="button"
                onClick={() => window.location.reload()}
                className="rounded-md bg-[var(--color-bad)] px-3 py-1.5 text-sm font-medium text-white hover:opacity-90"
              >
                {this.tr("errorboundary_reload_window", "Reload window")}
              </button>
              <button
                type="button"
                onClick={() => {
                  // Package the auto-collected reports and open Discord so the
                  // user can report this crash in one step. Dynamic import
                  // keeps the reporter out of the boundary's static deps.
                  // Browser: there's no host filesystem to zip a report onto
                  // (crash reports themselves are never written outside
                  // Tauri — see crashReporter.ts), so just open the channel
                  // instead of silently failing the zip step.
                  void import("../lib/reportProblem").then(async (m) => {
                    if (isTauriEnv()) {
                      await m.reportProblem(`crash-screen: ${err.name}: ${err.message}`);
                    } else {
                      await m.openReportChannel();
                    }
                  });
                }}
                className="rounded-md border border-[var(--color-accent)] px-3 py-1.5 text-sm font-medium text-[var(--color-accent)] hover:bg-[var(--color-surface)]"
              >
                {this.tr("errorboundary_report_crash", "Report this crash")}
              </button>
            </div>
            <div className="mt-3 text-xs text-[var(--color-muted)]">
              {this.tr(
                "errorboundary_recorded_in_logs",
                "This was saved to a crash report automatically. Click “Report this crash” to package it and post it on Discord so we can fix it.",
              )}
            </div>
          </div>
        </div>
      );
    }
    return this.props.children;
  }
}
