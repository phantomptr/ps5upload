import { trStatic } from "./trStatic";

export type Operation =
  | "browse-console"
  | "upload"
  | "install-package"
  | "manage-system"
  | "local-only";

export interface ReadinessContext {
  host: string;
  engineUp: boolean;
  helperUp: boolean;
  kernelRw: boolean | null;
}

export interface OperationReadiness {
  ready: boolean;
  blockers: string[];
  warnings: string[];
}

/** One operation-aware readiness vocabulary for buttons and preflight UI.
 * Connectivity is not a single boolean: local work only needs the engine,
 * console reads/writes need the helper, and privileged system changes also
 * need verified kernel R/W. */
/** The blocker for a console whose helper does not answer; callers that know why (rest mode)
 * swap it for a better reason. */
export function helperNotConnected(): string {
  return trStatic("readiness_helper_not_connected", "The PS5 helper is not connected.");
}

export function evaluateOperationReadiness(
  operation: Operation,
  context: ReadinessContext,
): OperationReadiness {
  const blockers: string[] = [];
  const warnings: string[] = [];

  if (!context.engineUp) blockers.push(trStatic("readiness_engine_offline", "The PS5Upload engine is offline."));
  if (operation !== "local-only") {
    if (!context.host.trim()) blockers.push(trStatic("readiness_no_console", "No PS5 is selected."));
    if (!context.helperUp) blockers.push(helperNotConnected());
  }
  if (operation === "manage-system") {
    if (context.kernelRw === false) {
      blockers.push(
        trStatic("readiness_kernel_rw_required", "Kernel read/write is required for this system action."),
      );
    } else if (context.kernelRw === null) {
      warnings.push(
        trStatic("readiness_kernel_rw_unverified", "Kernel read/write has not been verified yet."),
      );
    }
  }
  if (operation === "install-package" && context.kernelRw !== true) {
    warnings.push(
      trStatic(
        "readiness_package_unconfirmed",
        "Package compatibility cannot be fully confirmed until console readiness is checked.",
      ),
    );
  }

  return { ready: blockers.length === 0, blockers, warnings };
}
