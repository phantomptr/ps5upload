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
export function evaluateOperationReadiness(
  operation: Operation,
  context: ReadinessContext,
): OperationReadiness {
  const blockers: string[] = [];
  const warnings: string[] = [];

  if (!context.engineUp) blockers.push("The ps5upload engine is offline.");
  if (operation !== "local-only") {
    if (!context.host.trim()) blockers.push("No PS5 is selected.");
    if (!context.helperUp) blockers.push("The PS5 helper is not connected.");
  }
  if (operation === "manage-system") {
    if (context.kernelRw === false) {
      blockers.push("Kernel read/write is required for this system action.");
    } else if (context.kernelRw === null) {
      warnings.push("Kernel read/write has not been verified yet.");
    }
  }
  if (operation === "install-package" && context.kernelRw !== true) {
    warnings.push(
      "Package compatibility cannot be fully confirmed until console readiness is checked.",
    );
  }

  return { ready: blockers.length === 0, blockers, warnings };
}
