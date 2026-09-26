import { describe, expect, it } from "vitest";
import { evaluateOperationReadiness } from "./operationReadiness";

const ready = {
  host: "192.168.1.20",
  engineUp: true,
  helperUp: true,
  kernelRw: true,
};

describe("operation readiness", () => {
  it("lets local-only work proceed without a selected console", () => {
    expect(
      evaluateOperationReadiness("local-only", {
        host: "",
        engineUp: true,
        helperUp: false,
        kernelRw: null,
      }).ready,
    ).toBe(true);
  });

  it("explains every missing dependency for console work", () => {
    const result = evaluateOperationReadiness("upload", {
      host: "",
      engineUp: false,
      helperUp: false,
      kernelRw: null,
    });
    expect(result.ready).toBe(false);
    expect(result.blockers).toEqual([
      "The ps5upload engine is offline.",
      "No PS5 is selected.",
      "The PS5 helper is not connected.",
    ]);
  });

  it("blocks privileged system actions when kernel R/W is absent", () => {
    const result = evaluateOperationReadiness("manage-system", {
      ...ready,
      kernelRw: false,
    });
    expect(result.ready).toBe(false);
    expect(result.blockers[0]).toContain("Kernel read/write");
  });

  it("keeps install available but reports compatibility uncertainty", () => {
    const result = evaluateOperationReadiness("install-package", {
      ...ready,
      kernelRw: null,
    });
    expect(result.ready).toBe(true);
    expect(result.warnings).toHaveLength(1);
  });
});
