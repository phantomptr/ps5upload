// HDR Remote Play: the console's HDR stream (H.265 Main10, PQ, BT.2020; asked for with the
// engine's "h265_hdr" codec) shown in HDR on an HDR screen.
//
// What browsers give (checked against the specs and Chromium's source, 2026-10):
// - A decoded HDR VideoFrame drawn to a 2D canvas or uploaded to WebGL is converted to SDR by the
//   browser (PQ 203 nits = white, brighter clipped). Its raw PQ values are not reachable: a GPU
//   frame has no format and copyTo() reads back 8-bit RGB at most. So a WebGL shader cannot
//   tone-map the stream itself, and the HDR stream is offered only where it can be shown in HDR.
// - A WebGPU canvas configured with toneMapping { mode: "extended" } and a float format shows
//   values above 1.0 brighter than white, up to what the screen can do (Chrome 129+). Whether a
//   frame keeps its range on the way in (copyExternalImageToTexture into rgba16float) is up to
//   the browser; at worst it is the same SDR picture as WebGL's. Hence "experimental".
// Sharpening, upscaling and the colour tools stay off in HDR: they are SDR shaders (an 8-bit
// pass would undo the point of HDR). Crop to 21:9 and Sharp pixels still apply.

import { cropRect } from "./rpPicture";
import { withDecoderStart } from "./videoDecoderGuard";

/** What the console is asked for. */
export type StreamCodec = "h264" | "h265" | "h265_hdr";

/** Settings › Video format. */
export type CodecPref = "auto" | "h264" | "hdr";

/** Whether HDR can be offered here, or the first reason it cannot. */
export type HdrSupport = { ok: true } | { ok: false; why: "decoder" | "screen" | "display" };

/** HEVC Main10 as the PS5 sends it (level 5.1), then plainer forms. */
const MAIN10_CANDIDATES = ["hev1.2.4.L153.B0", "hvc1.2.4.L153.B0", "hev1.2.4.L150.B0", "hev1.2.4.L123.B0"];

/** The first HEVC Main10 codec string this device decodes, or null. */
export async function supportedHevcMain10(width: number, height: number): Promise<string | null> {
  if (typeof VideoDecoder === "undefined") return null;
  return withDecoderStart(async () => {
    for (const codec of MAIN10_CANDIDATES) {
      try {
        if ((await VideoDecoder.isConfigSupported({ codec, codedWidth: width, codedHeight: height })).supported) {
          return codec;
        }
      } catch {
        // Not this one.
      }
    }
    return null;
  });
}

/** The screen says it can show HDR (it may still be switched off in the system's settings). */
export function hdrScreen(win: Window | undefined = typeof window === "undefined" ? undefined : window): boolean {
  try {
    return !!win?.matchMedia?.("(dynamic-range: high)").matches;
  } catch {
    return false;
  }
}

/** The canvas set-up HDR is drawn with: half floats, Display P3, values above 1.0 kept. */
function canvasConfig(device: GPUDevice): GPUCanvasConfiguration {
  return {
    device,
    format: "rgba16float",
    colorSpace: "display-p3",
    toneMapping: { mode: "extended" },
    alphaMode: "opaque",
  };
}

/** A WebGPU device, or null where there is none. */
async function gpuDevice(): Promise<GPUDevice | null> {
  const gpu = typeof navigator === "undefined" ? undefined : (navigator as Navigator & { gpu?: GPU }).gpu;
  if (!gpu) return null;
  const adapter = await gpu.requestAdapter();
  return adapter ? await adapter.requestDevice() : null;
}

/** The canvas's WebGPU context (TypeScript's DOM types have no "webgpu" overload yet). */
function webgpuContext(c: HTMLCanvasElement): GPUCanvasContext | null {
  return c.getContext("webgpu" as "2d") as unknown as GPUCanvasContext | null;
}

/** GPUBufferUsage / GPUTextureUsage flags (WebGPU spec values; the constants are not in
 *  TypeScript's DOM types). */
const BUFFER_UNIFORM = 0x40;
const BUFFER_COPY_DST = 0x08;
const TEXTURE_COPY_DST = 0x02;
const TEXTURE_BINDING = 0x04;
const TEXTURE_RENDER_ATTACHMENT = 0x10;

/** Configure `ctx` for HDR and check the browser kept the extended range (getConfiguration()
 *  reports it only where it is supported; older browsers drop the unknown member). */
function configureHdr(ctx: GPUCanvasContext, device: GPUDevice): boolean {
  ctx.configure(canvasConfig(device));
  return ctx.getConfiguration?.()?.toneMapping?.mode === "extended";
}

let displayProbe: Promise<boolean> | null = null;

/** WebGPU can draw an extended-range canvas here (checked once a page). */
function hdrCanvasWorks(): Promise<boolean> {
  displayProbe ??= (async () => {
    try {
      const device = await gpuDevice();
      if (!device) return false;
      const ctx = webgpuContext(document.createElement("canvas"));
      const ok = !!ctx && configureHdr(ctx, device);
      ctx?.unconfigure();
      device.destroy();
      return ok;
    } catch {
      return false;
    }
  })();
  return displayProbe;
}

/** Whether this device can play the HDR stream in HDR: it decodes HEVC Main10, the screen is
 *  HDR, and WebGPU draws an extended-range canvas. */
export async function hdrSupport(width = 1920, height = 1080): Promise<HdrSupport> {
  if (!(await supportedHevcMain10(width, height))) return { ok: false, why: "decoder" };
  if (!hdrScreen()) return { ok: false, why: "screen" };
  if (!(await hdrCanvasWorks())) return { ok: false, why: "display" };
  return { ok: true };
}

/** What to ask the console for: H.264 when chosen, HDR when chosen and it works here, else HEVC
 *  where this device decodes it. `hdrOff` says why HDR was asked for but not used. */
export function pickCodec(
  pref: CodecPref,
  hevc: boolean,
  hdr: HdrSupport | null,
): { codec: StreamCodec; hdrOff?: string } {
  if (pref === "h264") return { codec: "h264" };
  const sdr: StreamCodec = hevc ? "h265" : "h264";
  if (pref !== "hdr") return { codec: sdr };
  if (hdr?.ok) return { codec: "h265_hdr" };
  return { codec: sdr, hdrOff: hdr ? hdr.why : "not checked" };
}

/** A frame the decoder says is HDR (PQ or HLG). */
export function isHdrFrame(cs: { transfer?: string | null } | null | undefined): boolean {
  return cs?.transfer === "pq" || cs?.transfer === "hlg";
}

const WGSL = `
struct U { src: vec4f }
@group(0) @binding(0) var s: sampler;
@group(0) @binding(1) var t: texture_2d<f32>;
@group(0) @binding(2) var<uniform> u: U;
struct V { @builtin(position) pos: vec4f, @location(0) uv: vec2f }
@vertex fn vs(@builtin(vertex_index) i: u32) -> V {
  var p = array(vec2f(-1.0, -1.0), vec2f(1.0, -1.0), vec2f(-1.0, 1.0), vec2f(1.0, 1.0));
  var o: V;
  o.pos = vec4f(p[i], 0.0, 1.0);
  let q = p[i] * 0.5 + 0.5;
  // Texture rows run top-down; u.src is the part shown (x, y, w, h in 0-1).
  o.uv = vec2f(u.src.x + q.x * u.src.z, u.src.y + (1.0 - q.y) * u.src.w);
  return o;
}
@fragment fn fs(v: V) -> @location(0) vec4f {
  return vec4f(textureSample(t, s, v.uv).rgb, 1.0);
}`;

/** Draws HDR frames onto a canvas through WebGPU with the extended range. The canvas takes the
 *  WebGPU context for good: a canvas holds one kind. */
export class HdrPresenter {
  private tex: GPUTexture | null = null;
  private bind: GPUBindGroup | null = null;
  private readonly uni: GPUBuffer;
  private readonly pipeline: GPURenderPipeline;
  private readonly sampler: GPUSampler;
  /** The device went away (driver reset, GPU unplugged): nothing more can be drawn. */
  lost = false;

  private constructor(
    readonly canvas: HTMLCanvasElement,
    private readonly ctx: GPUCanvasContext,
    private readonly device: GPUDevice,
  ) {
    const module = device.createShaderModule({ code: WGSL });
    this.pipeline = device.createRenderPipeline({
      layout: "auto",
      vertex: { module, entryPoint: "vs" },
      fragment: { module, entryPoint: "fs", targets: [{ format: "rgba16float" }] },
      primitive: { topology: "triangle-strip" },
    });
    this.sampler = device.createSampler({ magFilter: "linear", minFilter: "linear" });
    this.uni = device.createBuffer({ size: 16, usage: BUFFER_UNIFORM | BUFFER_COPY_DST });
    void device.lost.then(() => {
      this.lost = true;
    });
  }

  /** Set `canvas` up for HDR; throws (with why) where it cannot be. */
  static async create(canvas: HTMLCanvasElement): Promise<HdrPresenter> {
    const device = await gpuDevice();
    if (!device) throw new Error("no WebGPU device");
    const ctx = webgpuContext(canvas);
    if (!ctx) {
      device.destroy();
      throw new Error("no WebGPU canvas");
    }
    if (!configureHdr(ctx, device)) {
      ctx.unconfigure();
      device.destroy();
      throw new Error("the WebGPU canvas has no extended range");
    }
    return new HdrPresenter(canvas, ctx, device);
  }

  /** Draw `frame` (w×h, its 21:9 band when `crop219`); the canvas is sized to the result. */
  render(frame: VideoFrame, w: number, h: number, crop219: boolean): { w: number; h: number } {
    if (this.lost) throw new Error("the WebGPU device was lost");
    const d = this.device;
    const crop = cropRect(w, h, crop219);
    if (!this.tex || this.tex.width !== w || this.tex.height !== h) {
      this.tex?.destroy();
      this.tex = d.createTexture({
        size: [w, h],
        format: "rgba16float",
        usage: TEXTURE_BINDING | TEXTURE_COPY_DST | TEXTURE_RENDER_ATTACHMENT,
      });
      this.bind = d.createBindGroup({
        layout: this.pipeline.getBindGroupLayout(0),
        entries: [
          { binding: 0, resource: this.sampler },
          { binding: 1, resource: this.tex.createView() },
          { binding: 2, resource: { buffer: this.uni } },
        ],
      });
    }
    if (this.canvas.width !== w || this.canvas.height !== crop.h) {
      this.canvas.width = w;
      this.canvas.height = crop.h;
    }
    // Into a half-float texture in the canvas's colour space, where the range above white can
    // survive (the spec does not clamp; what each browser keeps is its own).
    d.queue.copyExternalImageToTexture({ source: frame }, { texture: this.tex, colorSpace: "display-p3" }, [w, h]);
    d.queue.writeBuffer(this.uni, 0, new Float32Array([0, crop.y / h, 1, crop.h / h]));
    const enc = d.createCommandEncoder();
    const pass = enc.beginRenderPass({
      colorAttachments: [
        { view: this.ctx.getCurrentTexture().createView(), loadOp: "clear", storeOp: "store", clearValue: [0, 0, 0, 1] },
      ],
    });
    pass.setPipeline(this.pipeline);
    pass.setBindGroup(0, this.bind);
    pass.draw(4);
    pass.end();
    d.queue.submit([enc.finish()]);
    return { w, h: crop.h };
  }

  dispose(): void {
    try {
      this.tex?.destroy();
      this.ctx.unconfigure();
      this.device.destroy();
    } catch {
      // Already gone.
    }
  }
}
