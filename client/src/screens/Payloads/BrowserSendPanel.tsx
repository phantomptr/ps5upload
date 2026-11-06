import { useState } from "react";
import { Button, ErrorCard, SuccessCard, Input } from "../../components";
import { useConnectionStore } from "../../state/connection";
import { sendBlockedByRest } from "./probeVerdict";
import { getEngineUrl } from "../../state/engine";
import { hostOf } from "../../lib/addr";
import { useTr } from "../../state/lang";
import { PayloadSendRefused } from "../../api/ps5";
import { payloadSendRefusalText } from "../../lib/payloadSendRefusal";

/** An explicit send gesture forwards ELF bytes; no browser filesystem paths or TCP sockets. */
export default function BrowserSendPanel() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const power = useConnectionStore((s) => s.power);
  const helperStatus = useConnectionStore((s) => s.payloadStatus);
  const asleep = sendBlockedByRest(host, host, power, helperStatus);
  const [file, setFile] = useState<File | null>(null);
  const [port, setPort] = useState("9021");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [sent, setSent] = useState<number | null>(null);
  async function send() {
    if (!file || !host) return;
    setBusy(true); setError(null); setSent(null);
    try {
      if (file.size < 4 || file.size > 64 * 1024 * 1024) throw new Error(tr("browserPayload.size", "Choose an ELF payload of 64 MiB or smaller."));
      const query = new URLSearchParams({ host: hostOf(host), port });
      const response = await fetch(`${getEngineUrl()}/api/payload/send?${query}`, {
        method: "POST", body: file, headers: { "content-type": "application/octet-stream" },
        signal: AbortSignal.timeout(120_000),
      });
      const body = await response.json() as { bytes_sent?: number; error?: string };
      // A refusal comes as a reason the app can say in the user's language.
      if (response.status === 409) throw new PayloadSendRefused(body.error ?? "loader_busy");
      if (!response.ok) throw new Error(body.error || `engine HTTP ${response.status}`);
      if (typeof body.bytes_sent !== "number") throw new Error("Invalid send response");
      setSent(body.bytes_sent);
    } catch (e) { setError(e instanceof PayloadSendRefused ? payloadSendRefusalText(e) : String(e)); }
    finally { setBusy(false); }
  }
  return <div className="space-y-4">
    <label className="block text-sm">{tr("browserPayload.file", "Payload file")}
      <input type="file" accept=".elf" disabled={busy} className="block mt-2" onChange={(e) => {
        setFile(e.currentTarget.files?.[0] ?? null); setSent(null); setError(null);
      }} />
    </label>
    <p className="text-xs text-[var(--color-muted)]">{tr("browserPayload.hint", "Send an ELF payload from this device through the server to the selected PS5. Start the ELF loader first. Sending bytes does not confirm that the payload started.")}</p>
    <Input label={tr("browserPayload.port", "Loader port")} type="number" min={1} max={65535} value={port} disabled={busy} onChange={(e) => setPort(e.target.value)} />
    {asleep && (
      <p className="text-xs text-[var(--color-muted)]" data-testid="send-rest-mode">
        {tr("sendpayload_rest_mode", undefined, "The PS5 is in rest mode. Wake it to send a payload.")}
      </p>
    )}
    <Button disabled={busy || asleep || !file || !host || !/^\d+$/.test(port) || Number(port) < 1 || Number(port) > 65535} onClick={() => void send()}>{tr("payloads_tab_send", undefined, "Send payload")}</Button>
    {sent != null && <SuccessCard title={tr("browserPayload.sent", { n: sent }, "Sent {n} bytes to the loader.")} />}
    {error && <ErrorCard title={error} />}
  </div>;
}
