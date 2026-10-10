/**
 * What the Send tab says about a picked payload, from `payload_probe`'s code
 * (client/src-tauri/src/commands/probes.rs).
 *
 * `blocked` means there is nothing sendable: the file couldn't be read, or it is
 * empty. Those once fell through to "Payload file looks OK" with Send enabled.
 */
export interface ProbeVerdict {
  key: string;
  fallback: string;
  vars?: Record<string, string>;
  blocked: boolean;
  /** Shown with a check rather than a warning. */
  good: boolean;
}

export function probeVerdict(code: string, isPs5upload: boolean, error?: string): ProbeVerdict {
  switch (code) {
    case "payload_probe_invalid_ext":
      return {
        key: "sendpayload_probe_invalid_ext",
        fallback: "Payload must be a .elf, .bin, .js, .lua, or .jar file.",
        blocked: false,
        good: false,
      };
    case "payload_probe_detected":
      return {
        key: "sendpayload_probe_detected",
        fallback: "This is a PS5Upload payload.",
        blocked: false,
        good: true,
      };
    case "payload_probe_no_signature":
      return isPs5upload
        ? {
            key: "sendpayload_probe_detected",
            fallback: "This is a PS5Upload payload.",
            blocked: false,
            good: true,
          }
        : {
            key: "sendpayload_probe_no_signature",
            fallback: "No PS5Upload signature found — use only if you trust this payload.",
            blocked: false,
            good: false,
          };
    case "payload_probe_read_error":
      return {
        key: "sendpayload_probe_read_error",
        fallback: "Couldn't read this file ({error}). Check that it still exists and can be opened, then pick it again.",
        vars: { error: error ?? "unknown error" },
        blocked: true,
        good: false,
      };
    case "payload_probe_too_small":
      return {
        key: "sendpayload_probe_too_small",
        fallback: "This file is empty, so there is nothing to send.",
        blocked: true,
        good: false,
      };
    default:
      return {
        key: "sendpayload_probe_ok",
        fallback: "Payload file looks OK.",
        blocked: false,
        good: isPs5upload,
      };
  }
}
