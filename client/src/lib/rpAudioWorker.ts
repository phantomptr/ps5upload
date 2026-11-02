// Remote Play sound decoded off the page's main thread (remotePlayStream.ts): the page hands each
// audio record from the engine here as it comes (the buffer transferred, not copied); this
// worker decodes it (rpAudioPipe.ts) and posts the samples straight to the audio worklet over
// a MessagePort the page gave it, so a busy page (a long draw, a big re-render) never holds
// the sound back. Where a worker has no AudioDecoder (older Safari, WebKitGTK without the
// codec), it says so at the probe and the page decodes as before.

import { OpusPipe } from "./rpAudioPipe";

interface Scope {
  onmessage: ((e: MessageEvent) => void) | null;
  postMessage(m: unknown): void;
}
const scope = self as unknown as Scope;

let out: MessagePort | null = null;
const pipe = new OpusPipe((planes, scale) => {
  out?.postMessage({ planes, scale }, planes.map((p) => p.buffer));
});

// Frames covered for missing ones, for the page's stats, when it changes.
let lastConcealed = 0;
setInterval(() => {
  if (pipe.concealed === lastConcealed) return;
  lastConcealed = pipe.concealed;
  scope.postMessage({ concealed: pipe.concealed });
}, 1000);

scope.onmessage = (e: MessageEvent) => {
  const d = e.data as { probe?: boolean; port?: MessagePort; reset?: boolean; close?: boolean } | ArrayBuffer;
  if (d instanceof ArrayBuffer) {
    pipe.record(new Uint8Array(d));
    return;
  }
  if (d.probe) {
    void probe().then((ok) => scope.postMessage({ ready: ok }));
    return;
  }
  if (d.port) {
    out?.close();
    out = d.port;
    return;
  }
  if (d.reset) {
    pipe.reset();
    return;
  }
  if (d.close) {
    pipe.reset();
    out?.close();
    out = null;
  }
};

async function probe(): Promise<boolean> {
  try {
    if (typeof AudioDecoder === "undefined" || typeof EncodedAudioChunk === "undefined") return false;
    const r = await AudioDecoder.isConfigSupported({ codec: "opus", sampleRate: 48000, numberOfChannels: 2 });
    return !!r.supported;
  } catch {
    return false;
  }
}
