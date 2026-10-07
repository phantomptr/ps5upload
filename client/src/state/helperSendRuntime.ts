import { bundledPayloadPath, payloadCheck, sendPayload } from "../api/ps5";
import { PS5_LOADER_PORT } from "../lib/addr";
import { isNotPairedError, reportIfNotPaired } from "../lib/consoleSession";
import { STUCK_LOADER_MESSAGE, waitForLoader } from "../lib/elfldrGuard";
import { useConnectionStore } from "./connection";
import { runHelperSend, type HelperSendResult } from "./helperSend";
import type { Translator } from "./lang";

const TIMEOUT_EN =
  "Payload didn't come up within 20s.{tail} Just send it again — a fresh send now force-evicts any stuck previous instance on its own, so you usually don't need to restart the PS5. If it still fails: kstuff may not be loaded yet (run First Run, or send kstuff first), the ELF crashed on boot, or the PS5 is unreachable.";

/** Sends the bundled helper to `host` and waits for it to answer (see state/helperSend), wired
 *  to the real console. One implementation for every place that offers "Send helper".
 *
 *  The connection store describes the selected console only, so a send that ends after the
 *  user moved to another console never labels that one. */
export function sendHelperTo(
  host: string,
  tr: Translator,
): Promise<HelperSendResult> {
  const target = host.trim();
  const conn = () => useConnectionStore.getState();
  return runHelperSend(
    target,
    {
      waitForLoader,
      bundledPath: bundledPayloadPath,
      send: sendPayload,
      check: payloadCheck,
      isNotPaired: isNotPairedError,
      sleep: (ms) => new Promise((r) => setTimeout(r, ms)),
      setProbing: (h, on) => {
        if (conn().host === h) conn().setStatus({ payloadProbing: on });
      },
      onUp: (h, status) => {
        if (conn().host !== h) return;
        conn().setStatus({
          payloadStatus: "up",
          payloadStatusHost: h,
          payloadVersion: status.payloadVersion,
          ps5Kernel: status.ps5Kernel,
          ucredElevated: status.ucredElevated,
          priorInstance: status.priorInstance,
          payloadProbing: false,
        });
      },
      onOk: (h, msg) => {
        if (conn().host === h) conn().setStep2("ok", msg);
      },
      onNotPaired: (h, error) => reportIfNotPaired(error, h),
    },
    {
      checkingLoader: tr(
        "connection_checking_loader",
        undefined,
        "Checking the PS5's elfldr…",
      ),
      stuck: tr("connection_elfldr_stuck", undefined, STUCK_LOADER_MESSAGE),
      sending: (elf) =>
        tr(
          "connection_sending_elf",
          { elf, host: target, port: PS5_LOADER_PORT },
          "Sending {elf} to {host}:{port}…",
        ),
      waiting: tr(
        "connection_waiting_boot",
        undefined,
        "Waiting for payload to boot…",
      ),
      running: tr(
        "connection_payload_running",
        { host: target },
        `Helper is running on ${target}`,
      ),
      timeout: (tail) => tr("connection_payload_timeout", { tail }, TIMEOUT_EN),
      notPaired: (error) =>
        error.toLowerCase().includes("different device")
          ? tr(
              "pairing_wrong_console_title",
              undefined,
              "A different PS5 answered at this address",
            )
          : tr("pairing_title", undefined, "Pair with your PS5"),
    },
  );
}
