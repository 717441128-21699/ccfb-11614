/**
 * 手写 WASM 装载与 C ABI 封装(无 wasm-bindgen 依赖)。
 *
 * 引擎通过线性内存交换数据:字体字节先写进 wasm 内存,
 * 栅格化后的 R8 图集也直接从同一段内存读取并上传 WebGL 纹理。
 */

/** 与 core/src/ffi.rs 的错误码保持一致。 */
export const ERR = {
  OK: 0,
  INVALID: -1,
  ATLAS_FULL: -2,
  GLYPH: -3,
  STATE: -4,
} as const;

interface TtExports {
  memory: WebAssembly.Memory;
  tt_init: (ptr: number, len: number, face: number, ppem: number, aw: number, ah: number) => number;
  tt_set_ppem: (ppem: number) => number;
  tt_glyph_id: (cp: number) => number;
  tt_render: (gid: number, outX: number, outY: number) => number;
  tt_entry_metrics: (
    gid: number,
    w: number, h: number,
    advance: number, lsb: number, top: number,
  ) => number;
  tt_atlas_ptr: () => number;
  tt_atlas_size: (w: number, h: number) => number;
  tt_atlas_clear: () => number;
  tt_line_metrics: (a: number, d: number, g: number) => number;
  tt_hint_stats: (op: number, unk: number, ab: number) => number;
  tt_shutdown: () => void;
  tt_version: () => number;
  tt_alloc_input: (len: number, outCap: number) => bigint;
  tt_scratch: () => bigint;
}

export interface EntryMetrics {
  x: number;
  y: number;
  width: number;
  height: number;
  advance: number;
  leftSideBearing: number;
  topBearing: number;
}

export interface LineMetrics {
  ascent: number;
  descent: number;
  lineGap: number;
}

export interface HintStats {
  opcodes: number;
  unknown: number;
  aborts: number;
}

export class TtEngine {
  private exports: TtExports;
  private memory: WebAssembly.Memory;
  /** 引擎出参暂存区地址(tt_scratch,128 字节)。 */
  private scratch: number;

  private constructor(instance: WebAssembly.Instance) {
    this.exports = instance.exports as unknown as TtExports;
    this.memory = this.exports.memory;
    this.scratch = Number(this.exports.tt_scratch());
  }

  static async load(wasmUrl: string | URL): Promise<TtEngine> {
    const resp = await fetch(wasmUrl.toString());
    if (!resp.ok) throw new Error(`无法加载 WASM: ${resp.status}`);
    const bytes = await resp.arrayBuffer();
    const { instance } = await WebAssembly.instantiate(bytes, {});
    return new TtEngine(instance);
  }

  version(): number {
    return this.exports.tt_version();
  }

  /** 把字体字节写入引擎分配的输入缓冲并初始化。 */
  initFont(font: Uint8Array, faceIndex: number, ppem: number, atlasW: number, atlasH: number): void {
    const capSlot = this.scratch + 96;
    const ptr = Number(this.exports.tt_alloc_input(font.byteLength, capSlot));
    const cap = new Uint32Array(this.memory.buffer)[capSlot >>> 2];
    if (ptr === 0 || cap < font.byteLength) {
      throw new Error('tt_alloc_input 容量不足');
    }
    new Uint8Array(this.memory.buffer, ptr, font.byteLength).set(font);
    const rc = this.exports.tt_init(ptr, font.byteLength, faceIndex, ppem, atlasW, atlasH);
    if (rc !== ERR.OK) throw new Error(`tt_init 失败,错误码 ${rc}`);
  }

  setPpem(ppem: number): void {
    const rc = this.exports.tt_set_ppem(ppem);
    if (rc !== ERR.OK) throw new Error(`tt_set_ppem 失败: ${rc}`);
  }

  glyphId(codepoint: number): number {
    return this.exports.tt_glyph_id(codepoint);
  }

  /** 栅格化并装入图集,返回条目度量。 */
  render(gid: number): EntryMetrics {
    // scratch 布局(128 字节):[0..4)=x [4..8)=y [8..)=度量
    const px = this.scratch;
    const py = this.scratch + 4;
    const rc = this.exports.tt_render(gid, px, py);
    if (rc !== ERR.OK) throw new Error(`tt_render(gid=${gid}) 失败: ${rc}`);
    const u32 = new Uint32Array(this.memory.buffer);
    const x = u32[px >>> 2];
    const y = u32[py >>> 2];

    const base = this.scratch + 8;
    const slots = {
      w: base, h: base + 4,
      advance: base + 8, lsb: base + 12, top: base + 16,
    };
    const rc2 = this.exports.tt_entry_metrics(
      gid, slots.w, slots.h, slots.advance, slots.lsb, slots.top,
    );
    if (rc2 !== ERR.OK) throw new Error(`tt_entry_metrics 失败: ${rc2}`);
    const f32 = new Float32Array(this.memory.buffer);
    const u32b = new Uint32Array(this.memory.buffer);
    return {
      x, y,
      width: u32b[slots.w >>> 2],
      height: u32b[slots.h >>> 2],
      advance: f32[slots.advance >>> 2],
      leftSideBearing: f32[slots.lsb >>> 2],
      topBearing: f32[slots.top >>> 2],
    };
  }

  /** 图集 R8 像素的视图(直接引用线性内存,调用方应立即使用或拷贝)。 */
  atlasPixels(): { data: Uint8Array; width: number; height: number } {
    const pW = this.scratch + 64;
    const pH = pW + 4;
    this.exports.tt_atlas_size(pW, pH);
    const u32 = new Uint32Array(this.memory.buffer);
    const width = u32[pW >>> 2];
    const height = u32[pH >>> 2];
    const ptr = this.exports.tt_atlas_ptr();
    const data = new Uint8Array(this.memory.buffer, ptr, width * height);
    return { data, width, height };
  }

  clearAtlas(): void {
    this.exports.tt_atlas_clear();
  }

  lineMetrics(): LineMetrics {
    const base = this.scratch + 80;
    this.exports.tt_line_metrics(base, base + 4, base + 8);
    const f32 = new Float32Array(this.memory.buffer);
    return {
      ascent: f32[base >>> 2],
      descent: f32[(base + 4) >>> 2],
      lineGap: f32[(base + 8) >>> 2],
    };
  }

  hintStats(): HintStats {
    // u64 需 8 字节对齐:base 取 scratch+96
    const base = this.scratch + 96;
    this.exports.tt_hint_stats(base, base + 8, base + 12);
    const u32 = new Uint32Array(this.memory.buffer);
    const u64 = new BigUint64Array(this.memory.buffer);
    return {
      opcodes: Number(u64[base >>> 3]),
      unknown: u32[(base + 8) >>> 2],
      aborts: u32[(base + 12) >>> 2],
    };
  }

  shutdown(): void {
    this.exports.tt_shutdown();
  }
}
