import { invoke } from "./invokeLogged";

/** The browser build's send: `payload_bundled_path` and `payload_send` are desktop-only commands,
 *  so the engine, which owns the bytes and the socket there, sends its own bundled helper. Throws when the engine
 *  could not (no helper image, loader not answering), so a caller that records "a helper was
 *  sent" (lib/helperSendGate) records nothing for a send that never happened. */
export async function engineSendsHelper(ip: string): Promise<void> {
  const r = (await invoke("payload_restore", { ip })) as {
    ok?: boolean;
    error?: string;
  };
  if (!r?.ok) throw new Error(r?.error ?? "the engine has no helper to send");
}
