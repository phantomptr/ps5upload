// Remote Play picture tools: sharpening, upscaling, brightness, contrast, saturation and a 21:9
// crop, done on the GPU (WebGL 2) as each decoded frame is drawn. The stream draws every frame
// through here straight onto its own canvas, so the page, its fit layout, screenshots and clips
// all see the result. With every tool off it is one plain pass: the picture looks as it did with
// the 2D canvas. "Sharp pixels" is CSS (image-rendering) and needs nothing here.
//
// Sharpening is RCAS and upscaling is EASU then RCAS, from AMD FidelityFX Super Resolution 1
// (ffx_fsr1.h, MIT, Advanced Micro Devices; LICENSES/FidelityFX-FSR-MIT.txt), ported to GLSL ES
// 3.0. It is plain shader code, so it runs the same on any GPU with WebGL 2.

/** Sharpening and upscaling: none, sharpen at the picture's own size (RCAS), or FSR 1 (EASU up
 *  to the picture's size on screen, then RCAS). */
export type Enhance = "off" | "sharpen" | "fsr";

export interface PictureTools {
  enhance: Enhance;
  /** 0–100: how much Sharpen and FSR sharpen. */
  sharpen: number;
  /** -50–50 each; 0 = unchanged. */
  brightness: number;
  contrast: number;
  saturation: number;
  /** Keep only the middle 21:9 band (a game letterboxed for ultrawide, on an ultrawide). */
  crop219: boolean;
  /** Scale with hard edges (retro games): CSS only. */
  pixelated: boolean;
}

export const DEFAULT_PICTURE: PictureTools = {
  enhance: "off",
  sharpen: 0,
  brightness: 0,
  contrast: 0,
  saturation: 0,
  crop219: false,
  pixelated: false,
};

/** Whether any tool changes the picture (pixelated is CSS): with none, the plain pass. */
export function needsPass(p: PictureTools): boolean {
  return (
    sharpens(p) || p.brightness !== 0 || p.contrast !== 0 || p.saturation !== 0 || p.crop219
  );
}

/** Sharpen or FSR is on (sharp pixels wants the pixels as they are). */
export function sharpens(p: PictureTools): boolean {
  return p.enhance !== "off" && !p.pixelated;
}

/** Upscaling is asked for and can apply. */
export function upscales(p: PictureTools): boolean {
  return p.enhance === "fsr" && !p.pixelated;
}

/** RCAS's sharpness in stops for a 0–100 strength: 0 is FSR's mild 2 stops, 100 its strongest. */
export function rcasStops(strength: number): number {
  return 2 - (2 * Math.min(100, Math.max(0, strength))) / 100;
}

/** Most an upscale goes: per side, and in all (4K). */
const UPSCALE_MAX_SIDE = 3;
const UPSCALE_MAX_PIXELS = 3840 * 2160;

/**
 * The size to upscale a `src` picture to, for a box of `box` device pixels on screen, or null
 * when it would gain nothing (the box is not at least 1.1× the picture). Never smaller than the
 * picture; at most 3× per side and 3840×2160 in all.
 */
export function upscaleSize(
  src: { w: number; h: number },
  box: { w: number; h: number } | null,
): { w: number; h: number } | null {
  if (!box || src.w <= 0 || src.h <= 0) return null;
  if (box.w < src.w * 1.1 && box.h < src.h * 1.1) return null;
  let w = Math.min(Math.max(box.w, src.w), src.w * UPSCALE_MAX_SIDE);
  let h = Math.min(Math.max(box.h, src.h), src.h * UPSCALE_MAX_SIDE);
  if (w * h > UPSCALE_MAX_PIXELS) {
    const k = Math.sqrt(UPSCALE_MAX_PIXELS / (w * h));
    w = Math.max(src.w, w * k);
    h = Math.max(src.h, h * k);
  }
  return { w: Math.round(w), h: Math.round(h) };
}

/** The part of a `w`×`h` frame shown: all of it, or the middle 21:9 band. */
export function cropRect(w: number, h: number, crop219: boolean): { y: number; h: number } {
  if (!crop219) return { y: 0, h };
  const band = Math.min(h, Math.round((w * 9) / 21 / 2) * 2);
  return { y: Math.round((h - band) / 2), h: band };
}

const ENHANCE: ReadonlyArray<Enhance> = ["off", "sharpen", "fsr"];

/** A stored setting, checked (anything odd is the default). Settings saved before Sharpening
 *  and upscaling was one choice keep their look: the Upscale switch is FSR, a sharpen amount
 *  alone is Sharpen. */
export function readPicture(v: unknown): PictureTools {
  const o = (v ?? {}) as Record<string, unknown>;
  const n = (x: unknown, lo: number, hi: number) =>
    typeof x === "number" && Number.isFinite(x) ? Math.min(hi, Math.max(lo, Math.round(x))) : 0;
  const sharpen = n(o.sharpen, 0, 100);
  const enhance: Enhance = ENHANCE.includes(o.enhance as Enhance)
    ? (o.enhance as Enhance)
    : o.upscale === true
      ? "fsr"
      : sharpen > 0
        ? "sharpen"
        : "off";
  return {
    enhance,
    sharpen,
    brightness: n(o.brightness, -50, 50),
    contrast: n(o.contrast, -50, 50),
    saturation: n(o.saturation, -50, 50),
    crop219: o.crop219 === true,
    pixelated: o.pixelated === true,
  };
}

/** A decoded frame's pixels as YUV planes in memory (VideoFrame.copyTo), drawn by the colour
 *  pass itself: WebKitGTK hands the stream's frames over as NV12 in system memory and converts
 *  them on the CPU when they are given to texImage2D, which costs it a 1080p frame's whole time
 *  at 60 fps. Uploading the planes as they are and converting on the GPU costs a few ms. */
export type PlaneFormat = "NV12" | "I420";

export interface Planes {
  format: PlaneFormat;
  /** Visible size in pixels (the planes are this, the chroma ones half of it rounded up). */
  w: number;
  h: number;
  data: Uint8Array;
  layout: ReadonlyArray<{ offset: number; stride: number }>;
  colorSpace: { matrix?: string | null; fullRange?: boolean | null } | null;
}

/** Whether a VideoFrame format is one the colour pass reads as planes. */
export function isPlaneFormat(f: string | null | undefined): f is PlaneFormat {
  return f === "NV12" || f === "I420";
}

/** A tightly packed layout for a `w`×`h` frame's planes (Y, then UV or U and V), and the bytes
 *  it takes: what copyTo is asked to write, so the upload knows every row's place. */
export function planeLayout(
  format: PlaneFormat,
  w: number,
  h: number,
): { layout: Array<{ offset: number; stride: number }>; size: number } {
  const cw = Math.ceil(w / 2);
  const ch = Math.ceil(h / 2);
  const y = w * h;
  if (format === "NV12") return { layout: [{ offset: 0, stride: w }, { offset: y, stride: cw * 2 }], size: y + cw * 2 * ch };
  return {
    layout: [{ offset: 0, stride: w }, { offset: y, stride: cw }, { offset: y + cw * ch, stride: cw }],
    size: y + 2 * cw * ch,
  };
}

/** Copies frames' planes out (VideoFrame.copyTo) into one buffer kept between frames, so a
 *  stream allocates nothing per frame. One copy at a time: read() again only after the last
 *  one's planes were drawn. */
export class PlaneReader {
  private buf = new Uint8Array(0);

  /** The frame's visible pixels as planes, or null when it is not NV12/I420. */
  async read(frame: VideoFrame): Promise<Planes | null> {
    const format = frame.format;
    if (!isPlaneFormat(format)) return null;
    const r = frame.visibleRect;
    const w = r?.width ?? frame.codedWidth;
    const h = r?.height ?? frame.codedHeight;
    const { layout, size } = planeLayout(format, w, h);
    if (this.buf.byteLength < size) this.buf = new Uint8Array(size);
    await frame.copyTo(this.buf, { layout });
    return { format, w, h, data: this.buf, layout, colorSpace: frame.colorSpace };
  }
}

/** YUV → RGB for a frame's colour space: rgb = m · (yuv − off), m column-major (GLSL mat3).
 *  BT.709 unless the frame says BT.601 or BT.2020; limited range unless it says full. */
export function yuvToRgb(cs: Planes["colorSpace"]): { m: number[]; off: [number, number, number] } {
  const mx = cs?.matrix;
  const [kr, kb] =
    mx === "smpte170m" || mx === "bt470bg" ? [0.299, 0.114] : mx === "bt2020-ncl" ? [0.2627, 0.0593] : [0.2126, 0.0722];
  const kg = 1 - kr - kb;
  const full = cs?.fullRange === true;
  const ys = full ? 1 : 255 / 219;
  const cs_ = full ? 1 : 255 / 224;
  const yo = full ? 0 : 16 / 255;
  const rv = 2 * (1 - kr) * cs_;
  const bu = 2 * (1 - kb) * cs_;
  const gu = ((-2 * kb * (1 - kb)) / kg) * cs_;
  const gv = ((-2 * kr * (1 - kr)) / kg) * cs_;
  // Columns: what Y, U and V each add to R, G, B.
  return { m: [ys, ys, ys, 0, gu, bu, rv, gv, 0], off: [yo, 128 / 255, 128 / 255] };
}

const VS = `#version 300 es
in vec2 p;
out vec2 uv;
uniform vec4 src; // x, y, w, h of the frame shown, in 0-1
void main() {
  uv = src.xy + (p * 0.5 + 0.5) * src.zw;
  uv.y = src.y + src.w - (uv.y - src.y); // frames are top-down
  gl_Position = vec4(p, 0.0, 1.0);
}`;

/** The colour pass: the frame (RGB, or YUV planes converted here) with brightness, contrast and
 *  saturation. `src` is how the pixel is read. */
const fsColour = (decl: string, src: string) => `#version 300 es
precision mediump float;
in vec2 uv;
out vec4 o;
${decl}
uniform float bright; // -0.5-0.5
uniform float contr;  // 0.5-1.5
uniform float satur;  // 0-2
void main() {
  vec3 c = ${src};
  c = (c - 0.5) * contr + 0.5 + bright;
  float l = dot(c, vec3(0.2126, 0.7152, 0.0722));
  c = mix(vec3(l), c, satur);
  o = vec4(clamp(c, 0.0, 1.0), 1.0);
}`;

const FS = fsColour("uniform sampler2D t;", "texture(t, uv).rgb");

const YUV_DECL = `uniform sampler2D t;  // Y
uniform sampler2D t1; // UV (NV12) or U (I420)
uniform sampler2D t2; // V (I420)
uniform mat3 yuvM;
uniform vec3 yuvOff;`;

/** NV12: Y, then U and V interleaved in one half-size plane. */
const FS_NV12 = fsColour(YUV_DECL, "yuvM * (vec3(texture(t, uv).r, texture(t1, uv).rg) - yuvOff)");
/** I420: Y, U and V each in a plane. */
const FS_I420 = fsColour(YUV_DECL, "yuvM * (vec3(texture(t, uv).r, texture(t1, uv).r, texture(t2, uv).r) - yuvOff)");

/** A full-screen triangle strip in pixel space (no flip: these passes read and write GL space). */
const VS_PX = `#version 300 es
in vec2 p;
void main() {
  gl_Position = vec4(p, 0.0, 1.0);
}`;

/** FSR 1 EASU (edge-adaptive spatial upsampling), ported from ffx_fsr1.h FsrEasuF: a 12-tap
 *  Lanczos-like filter steered along the local edge, clamped to its 2×2 neighbourhood. */
const FS_EASU = `#version 300 es
precision highp float;
out vec4 o;
uniform sampler2D t;
uniform vec2 scale; // input size / output size
uniform ivec2 maxp; // input size - 1

vec3 at(ivec2 q) { return texelFetch(t, clamp(q, ivec2(0), maxp), 0).rgb; }
float luma(vec3 c) { return c.b * 0.5 + (c.r * 0.5 + c.g); }

void set(inout vec2 dir, inout float len, float w, float lA, float lB, float lC, float lD, float lE) {
  float dc = lD - lC;
  float cb = lC - lB;
  float lenX = max(abs(dc), abs(cb));
  lenX = lenX > 0.0 ? 1.0 / lenX : 0.0;
  float dirX = lD - lB;
  dir.x += dirX * w;
  lenX = clamp(abs(dirX) * lenX, 0.0, 1.0);
  lenX *= lenX;
  len += lenX * w;
  float ec = lE - lC;
  float ca = lC - lA;
  float lenY = max(abs(ec), abs(ca));
  lenY = lenY > 0.0 ? 1.0 / lenY : 0.0;
  float dirY = lE - lA;
  dir.y += dirY * w;
  lenY = clamp(abs(dirY) * lenY, 0.0, 1.0);
  lenY *= lenY;
  len += lenY * w;
}

void tap(inout vec3 aC, inout float aW, vec2 off, vec2 dir, vec2 len2, float lob, float clp, vec3 c) {
  vec2 v = vec2(off.x * dir.x + off.y * dir.y, off.x * (-dir.y) + off.y * dir.x) * len2;
  float d2 = min(v.x * v.x + v.y * v.y, clp);
  float wB = 2.0 / 5.0 * d2 - 1.0;
  float wA = lob * d2 - 1.0;
  wB *= wB;
  wA *= wA;
  wB = 25.0 / 16.0 * wB - (25.0 / 16.0 - 1.0);
  float w = wB * wA;
  aC += c * w;
  aW += w;
}

void main() {
  vec2 pp = gl_FragCoord.xy * scale - 0.5;
  vec2 fp = floor(pp);
  pp -= fp;
  ivec2 f0 = ivec2(fp);
  //    b c
  //  e f g h
  //  i j k l
  //    n o
  vec3 b = at(f0 + ivec2(0, -1)), c = at(f0 + ivec2(1, -1));
  vec3 e = at(f0 + ivec2(-1, 0)), f = at(f0), g = at(f0 + ivec2(1, 0)), h = at(f0 + ivec2(2, 0));
  vec3 i = at(f0 + ivec2(-1, 1)), j = at(f0 + ivec2(0, 1)), k = at(f0 + ivec2(1, 1)), l = at(f0 + ivec2(2, 1));
  vec3 n = at(f0 + ivec2(0, 2)), oo = at(f0 + ivec2(1, 2));
  float bL = luma(b), cL = luma(c), eL = luma(e), fL = luma(f), gL = luma(g), hL = luma(h);
  float iL = luma(i), jL = luma(j), kL = luma(k), lL = luma(l), nL = luma(n), oL = luma(oo);

  vec2 dir = vec2(0.0);
  float len = 0.0;
  set(dir, len, (1.0 - pp.x) * (1.0 - pp.y), bL, eL, fL, gL, jL);
  set(dir, len, pp.x * (1.0 - pp.y), cL, fL, gL, hL, kL);
  set(dir, len, (1.0 - pp.x) * pp.y, fL, iL, jL, kL, nL);
  set(dir, len, pp.x * pp.y, gL, jL, kL, lL, oL);

  vec2 dir2 = dir * dir;
  float dirR = dir2.x + dir2.y;
  bool zro = dirR < 1.0 / 32768.0;
  dirR = zro ? 1.0 : inversesqrt(dirR);
  dir.x = zro ? 1.0 : dir.x;
  dir *= dirR;
  len = len * 0.5;
  len *= len;
  float stretch = (dir.x * dir.x + dir.y * dir.y) / max(abs(dir.x), abs(dir.y));
  vec2 len2 = vec2(1.0 + (stretch - 1.0) * len, 1.0 - 0.5 * len);
  float lob = 0.5 + ((1.0 / 4.0 - 0.04) - 0.5) * len;
  float clp = 1.0 / lob;

  vec3 aC = vec3(0.0);
  float aW = 0.0;
  tap(aC, aW, vec2(0.0, -1.0) - pp, dir, len2, lob, clp, b);
  tap(aC, aW, vec2(1.0, -1.0) - pp, dir, len2, lob, clp, c);
  tap(aC, aW, vec2(-1.0, 1.0) - pp, dir, len2, lob, clp, i);
  tap(aC, aW, vec2(0.0, 1.0) - pp, dir, len2, lob, clp, j);
  tap(aC, aW, vec2(0.0, 0.0) - pp, dir, len2, lob, clp, f);
  tap(aC, aW, vec2(-1.0, 0.0) - pp, dir, len2, lob, clp, e);
  tap(aC, aW, vec2(1.0, 1.0) - pp, dir, len2, lob, clp, k);
  tap(aC, aW, vec2(2.0, 1.0) - pp, dir, len2, lob, clp, l);
  tap(aC, aW, vec2(2.0, 0.0) - pp, dir, len2, lob, clp, h);
  tap(aC, aW, vec2(1.0, 0.0) - pp, dir, len2, lob, clp, g);
  tap(aC, aW, vec2(1.0, 2.0) - pp, dir, len2, lob, clp, oo);
  tap(aC, aW, vec2(0.0, 2.0) - pp, dir, len2, lob, clp, n);

  vec3 mn = min(min(f, g), min(j, k));
  vec3 mx = max(max(f, g), max(j, k));
  o = vec4(clamp(aC / aW, mn, mx), 1.0);
}`;

/** FSR 1 RCAS (robust contrast-adaptive sharpening), ported from ffx_fsr1.h FsrRcasF with its
 *  noise damping (compressed video): sharpens as far as it can without clipping. */
const FS_RCAS = `#version 300 es
precision highp float;
out vec4 o;
uniform sampler2D t;
uniform ivec2 maxp;
uniform float sharp; // exp2(-stops)

vec3 at(ivec2 q) { return texelFetch(t, clamp(q, ivec2(0), maxp), 0).rgb; }
float luma(vec3 c) { return c.b * 0.5 + (c.r * 0.5 + c.g); }

void main() {
  ivec2 q = ivec2(gl_FragCoord.xy);
  //   b
  // d e f
  //   h
  vec3 b = at(q + ivec2(0, -1)), d = at(q + ivec2(-1, 0)), e = at(q);
  vec3 f = at(q + ivec2(1, 0)), h = at(q + ivec2(0, 1));
  float bL = luma(b), dL = luma(d), eL = luma(e), fL = luma(f), hL = luma(h);
  float nz = 0.25 * (bL + dL + fL + hL) - eL;
  float range = max(max(max(bL, dL), max(eL, fL)), hL) - min(min(min(bL, dL), min(eL, fL)), hL);
  nz = range > 0.0 ? clamp(abs(nz) / range, 0.0, 1.0) : 0.0;
  nz = -0.5 * nz + 1.0;
  vec3 mn4 = min(min(b, d), min(f, h));
  vec3 mx4 = max(max(b, d), max(f, h));
  vec3 hitMin = mn4 / max(4.0 * mx4, vec3(1.0 / 32768.0));
  vec3 hitMax = (1.0 - mx4) / min(4.0 * mn4 - 4.0, vec3(-1.0 / 32768.0));
  vec3 lobeRGB = max(-hitMin, hitMax);
  float lobe = max(-(0.25 - 1.0 / 16.0), min(max(max(lobeRGB.r, lobeRGB.g), lobeRGB.b), 0.0)) * sharp;
  lobe *= nz;
  float rcpL = 1.0 / (4.0 * lobe + 1.0);
  o = vec4(clamp((lobe * (b + d + f + h) + e) * rcpL, 0.0, 1.0), 1.0);
}`;

function isPlanes(f: TexImageSource | Planes): f is Planes {
  return (f as Planes).data instanceof Uint8Array && Array.isArray((f as Planes).layout);
}

/** A texture to render into and read back from, at a size. */
interface Target {
  tex: WebGLTexture;
  fb: WebGLFramebuffer;
  w: number;
  h: number;
}

/** The frame drawer: draws each frame through the shaders straight onto the stream's canvas.
 *  Throws when the canvas gives no WebGL 2 (or a shader does not build). */
export class PicturePass {
  private gl: WebGL2RenderingContext;
  private prog: WebGLProgram;
  private tex: WebGLTexture;
  private u: Record<string, WebGLUniformLocation | null> = {};
  /** RCAS (Sharpen and FSR) and EASU (FSR), made when first used. */
  private rcas: { prog: WebGLProgram; t: WebGLUniformLocation | null; max: WebGLUniformLocation | null; sharp: WebGLUniformLocation | null } | null = null;
  private easu: { prog: WebGLProgram; t: WebGLUniformLocation | null; scale: WebGLUniformLocation | null; max: WebGLUniformLocation | null } | null = null;
  /** The colour pass for YUV planes (NV12 or I420), made when first used, and the planes'
   *  textures (Y, then UV or U and V) with the size each holds. */
  private yuv: Partial<Record<PlaneFormat, { prog: WebGLProgram; u: Record<string, WebGLUniformLocation | null> }>> = {};
  private planeTex: Array<{ tex: WebGLTexture; w: number; h: number; fmt: number }> = [];
  /** The textures between the passes: the colour pass's output (a), EASU's (b). */
  private a: Target | null = null;
  private b: Target | null = null;

  constructor(readonly canvas: HTMLCanvasElement | OffscreenCanvas) {
    // preserveDrawingBuffer: a screenshot (toBlob) and a clip (captureStream) read the canvas
    // at any time, not only right after a frame is drawn.
    const gl = canvas.getContext("webgl2", {
      alpha: false,
      antialias: false,
      depth: false,
      stencil: false,
      preserveDrawingBuffer: true,
    }) as WebGL2RenderingContext | null;
    if (!gl) throw new Error("no WebGL 2");
    this.gl = gl;
    this.prog = this.program(VS, FS);
    gl.useProgram(this.prog);
    const buf = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, buf);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);
    gl.enableVertexAttribArray(0);
    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);
    this.tex = gl.createTexture()!;
    gl.bindTexture(gl.TEXTURE_2D, this.tex);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    for (const n of ["src", "bright", "contr", "satur", "t"]) this.u[n] = gl.getUniformLocation(this.prog, n);
  }

  /** The context went away (webglcontextlost): nothing can be drawn until a new pass is made. */
  get lost(): boolean {
    return this.gl.isContextLost();
  }

  /** A linked program; the corner positions are attribute 0 in every one. */
  private program(vs: string, fs: string): WebGLProgram {
    const gl = this.gl;
    const sh = (type: number, src: string) => {
      const s = gl.createShader(type)!;
      gl.shaderSource(s, src);
      gl.compileShader(s);
      if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s) ?? "shader");
      return s;
    };
    const prog = gl.createProgram()!;
    gl.attachShader(prog, sh(gl.VERTEX_SHADER, vs));
    gl.attachShader(prog, sh(gl.FRAGMENT_SHADER, fs));
    gl.bindAttribLocation(prog, 0, "p");
    gl.linkProgram(prog);
    if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(prog) ?? "program");
    return prog;
  }

  private target(): Target {
    const gl = this.gl;
    const tex = gl.createTexture()!;
    gl.bindTexture(gl.TEXTURE_2D, tex);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    const fb = gl.createFramebuffer()!;
    return { tex, fb, w: 0, h: 0 };
  }

  /** `t` sized `w`×`h` and bound for drawing into. */
  private drawInto(t: Target, w: number, h: number): void {
    const gl = this.gl;
    if (t.w !== w || t.h !== h) {
      gl.bindTexture(gl.TEXTURE_2D, t.tex);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, w, h, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
      gl.bindFramebuffer(gl.FRAMEBUFFER, t.fb);
      gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, t.tex, 0);
      t.w = w;
      t.h = h;
    }
    gl.bindFramebuffer(gl.FRAMEBUFFER, t.fb);
    gl.viewport(0, 0, w, h);
  }

  /** Draw onto the canvas itself. */
  private drawToCanvas(w: number, h: number): void {
    this.gl.bindFramebuffer(this.gl.FRAMEBUFFER, null);
    this.gl.viewport(0, 0, w, h);
  }

  private rcasProgram(): NonNullable<PicturePass["rcas"]> {
    if (!this.rcas) {
      const gl = this.gl;
      const prog = this.program(VS_PX, FS_RCAS);
      this.rcas = {
        prog,
        t: gl.getUniformLocation(prog, "t"),
        max: gl.getUniformLocation(prog, "maxp"),
        sharp: gl.getUniformLocation(prog, "sharp"),
      };
    }
    return this.rcas;
  }

  private easuProgram(): NonNullable<PicturePass["easu"]> {
    if (!this.easu) {
      const gl = this.gl;
      const prog = this.program(VS_PX, FS_EASU);
      this.easu = {
        prog,
        t: gl.getUniformLocation(prog, "t"),
        scale: gl.getUniformLocation(prog, "scale"),
        max: gl.getUniformLocation(prog, "maxp"),
      };
    }
    return this.easu;
  }

  private yuvProgram(format: PlaneFormat): { prog: WebGLProgram; u: Record<string, WebGLUniformLocation | null> } {
    let y = this.yuv[format];
    if (!y) {
      const gl = this.gl;
      const prog = this.program(VS, format === "NV12" ? FS_NV12 : FS_I420);
      const u: Record<string, WebGLUniformLocation | null> = {};
      for (const n of ["src", "bright", "contr", "satur", "t", "t1", "t2", "yuvM", "yuvOff"]) u[n] = gl.getUniformLocation(prog, n);
      y = this.yuv[format] = { prog, u };
    }
    return y;
  }

  /** Upload the planes to texture units 0.., each its own size (R8, or RG8 for NV12's UV). */
  private uploadPlanes(f: Planes): void {
    const gl = this.gl;
    const cw = Math.ceil(f.w / 2);
    const ch = Math.ceil(f.h / 2);
    const planes =
      f.format === "NV12"
        ? [
            { w: f.w, h: f.h, fmt: gl.RED, ifmt: gl.R8, px: 1 },
            { w: cw, h: ch, fmt: gl.RG, ifmt: gl.RG8, px: 2 },
          ]
        : [
            { w: f.w, h: f.h, fmt: gl.RED, ifmt: gl.R8, px: 1 },
            { w: cw, h: ch, fmt: gl.RED, ifmt: gl.R8, px: 1 },
            { w: cw, h: ch, fmt: gl.RED, ifmt: gl.R8, px: 1 },
          ];
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
    planes.forEach((pl, i) => {
      const t = this.planeTex[i];
      gl.activeTexture(gl.TEXTURE0 + i);
      if (!t || t.w !== pl.w || t.h !== pl.h || t.fmt !== pl.ifmt) {
        // Fixed-size storage (texStorage2D): a new size is a new texture.
        if (t) gl.deleteTexture(t.tex);
        const tex = gl.createTexture()!;
        gl.bindTexture(gl.TEXTURE_2D, tex);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
        gl.texStorage2D(gl.TEXTURE_2D, 1, pl.ifmt, pl.w, pl.h);
        this.planeTex[i] = { tex, w: pl.w, h: pl.h, fmt: pl.ifmt };
      } else gl.bindTexture(gl.TEXTURE_2D, t.tex);
      const l = f.layout[i];
      gl.pixelStorei(gl.UNPACK_ROW_LENGTH, l.stride / pl.px);
      gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, pl.w, pl.h, pl.fmt, gl.UNSIGNED_BYTE, f.data, l.offset);
    });
    gl.pixelStorei(gl.UNPACK_ROW_LENGTH, 0);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 4);
    gl.activeTexture(gl.TEXTURE0);
  }

  /** Draw `frame` (w×h) with `p` onto the canvas, which is sized to the result (returned).
   *  `to`: the size to upscale to (upscaleSize), or null for the picture's own size. */
  render(
    frame: TexImageSource | Planes,
    w: number,
    h: number,
    p: PictureTools,
    to: { w: number; h: number } | null = null,
  ): { w: number; h: number } {
    const gl = this.gl;
    const crop = cropRect(w, h, p.crop219);
    const up = !!to && upscales(p);
    const sharp = sharpens(p);
    const out = up && to ? to : { w, h: crop.h };
    if (this.canvas.width !== out.w || this.canvas.height !== out.h) {
      this.canvas.width = out.w;
      this.canvas.height = out.h;
    }
    // The colour pass, at the picture's own size: onto the canvas, or into the first texture
    // for the sharpening and upscaling passes.
    if (sharp) this.drawInto((this.a ??= this.target()), w, crop.h);
    else this.drawToCanvas(w, crop.h);
    let u = this.u;
    if (isPlanes(frame)) {
      const y = this.yuvProgram(frame.format);
      u = y.u;
      gl.useProgram(y.prog);
      this.uploadPlanes(frame);
      const c = yuvToRgb(frame.colorSpace);
      gl.uniformMatrix3fv(u.yuvM, false, c.m);
      gl.uniform3f(u.yuvOff, c.off[0], c.off[1], c.off[2]);
      gl.uniform1i(u.t1, 1);
      gl.uniform1i(u.t2, 2);
    } else {
      gl.useProgram(this.prog);
      gl.activeTexture(gl.TEXTURE0);
      gl.bindTexture(gl.TEXTURE_2D, this.tex);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, frame);
    }
    gl.uniform4f(u.src, 0, crop.y / h, 1, crop.h / h);
    gl.uniform1f(u.bright, p.brightness / 100);
    gl.uniform1f(u.contr, 1 + p.contrast / 100);
    gl.uniform1f(u.satur, 1 + p.saturation / 50);
    gl.uniform1i(u.t, 0);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
    if (!sharp || !this.a) return out;

    let from = this.a;
    if (up) {
      // EASU: to the screen's size, into the second texture.
      const easu = this.easuProgram();
      this.drawInto((this.b ??= this.target()), out.w, out.h);
      gl.useProgram(easu.prog);
      gl.bindTexture(gl.TEXTURE_2D, this.a.tex);
      gl.uniform1i(easu.t, 0);
      gl.uniform2f(easu.scale, w / out.w, crop.h / out.h);
      gl.uniform2i(easu.max, w - 1, crop.h - 1);
      gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
      from = this.b;
    }

    // RCAS: sharpened onto the canvas.
    const rcas = this.rcasProgram();
    this.drawToCanvas(out.w, out.h);
    gl.useProgram(rcas.prog);
    gl.bindTexture(gl.TEXTURE_2D, from.tex);
    gl.uniform1i(rcas.t, 0);
    gl.uniform2i(rcas.max, out.w - 1, out.h - 1);
    gl.uniform1f(rcas.sharp, Math.pow(2, -rcasStops(p.sharpen)));
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
    return out;
  }

  /** Free the GPU objects (the canvas keeps its context: it cannot take another kind). */
  dispose(): void {
    const gl = this.gl;
    if (gl.isContextLost()) return;
    gl.deleteTexture(this.tex);
    for (const t of this.planeTex) gl.deleteTexture(t.tex);
    for (const y of Object.values(this.yuv)) if (y) gl.deleteProgram(y.prog);
    for (const t of [this.a, this.b]) {
      if (!t) continue;
      gl.deleteTexture(t.tex);
      gl.deleteFramebuffer(t.fb);
    }
    gl.deleteProgram(this.prog);
    if (this.rcas) gl.deleteProgram(this.rcas.prog);
    if (this.easu) gl.deleteProgram(this.easu.prog);
  }
}
