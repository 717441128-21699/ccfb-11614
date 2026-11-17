/**
 * Thin typed wrapper over the C ABI exported by the WebAssembly core.
 * No bindings generator: the module exports plain functions and one linear
 * memory, and all font/glyph buffers live inside that memory.
 */

export interface WasmExports extends WebAssembly.Exports {
  memory: WebAssembly.Memory;
  tt_font_open(dataPtr: number, dataLen: number, fontIndex: number): number;
  tt_font_close(handle: number): void;
  tt_num_glyphs(handle: number): number;
  tt_units_per_em(handle: number): number;
  tt_codepoint_to_gid(handle: number, codepoint: number): number;
  tt_set_pixel_size(handle: number, ppem: number, hinting: number): number;
  tt_render_glyph(handle: number, gid: number, outPtr: number): number;
  tt_alpha_free(ptr: number, len: number): void;
  tt_line_metrics(handle: number, outPtr: number): number;
  tt_last_error_message(out: number, cap: number): number;
  tt_alloc(size: number): number;
  tt_free(ptr: number, size: number, cap: number): void;
}

/** Mirrors the `GlyphResult` repr(C) block (8 x u32/f32 = 32 bytes). */
export interface GlyphResult {
  width: number;
  height: number;
  left: number;
  top: number;
  advance: number;
  lsb: number;
  alphaPtr: number;
  alphaLen: number;
}

/** Mirrors `LineMetrics` (3 x f32 = 12 bytes). */
export interface LineMetrics {
  ascender: number;
  descender: number;
  lineGap: number;
}

export class WasmError extends Error {
  constructor(
    public readonly code: number,
    message: string,
  ) {
    super(`ttfont core error ${code}: ${message}`);
    this.name = 'WasmError';
  }
}

export class WasmCore {
  readonly exports: WasmExports;
  readonly scratchPtr: number;
  private static readonly SCRATCH_SIZE = 64;

  private constructor(exports: WasmExports) {
    this.exports = exports;
    this.scratchPtr = exports.tt_alloc(WasmCore.SCRATCH_SIZE);
    if (this.scratchPtr === 0) throw new Error('wasm scratch alloc failed');
  }

  static async instantiate(
    source: BufferSource | Response | Promise<BufferSource | Response>,
  ): Promise<WasmCore> {
    const module = await WebAssembly.instantiate(await source, {});
    const instance =
      module instanceof WebAssembly.Instance
        ? module
        : (module as WebAssembly.WebAssemblyInstantiatedSource).instance;
    return new WasmCore(instance.exports as unknown as WasmExports);
  }

  check(code: number): void {
    if (code !== 0) throw new WasmError(code, this.lastError());
  }

  private lastError(): string {
    const cap = 96;
    const ptr = this.exports.tt_alloc(cap);
    try {
      const n = this.exports.tt_last_error_message(ptr, cap);
      const bytes = new Uint8Array(this.exports.memory.buffer, ptr, n);
      return new TextDecoder().decode(bytes);
    } finally {
      this.exports.tt_free(ptr, cap, cap);
    }
  }

  openFont(data: Uint8Array, fontIndex = 0): FontHandle {
    const ptr = this.exports.tt_alloc(data.byteLength);
    if (ptr === 0) throw new Error('wasm alloc failed for font bytes');
    new Uint8Array(this.exports.memory.buffer, ptr, data.byteLength).set(data);
    const handle = this.exports.tt_font_open(ptr, data.byteLength, fontIndex);
    // The core copies the bytes; the staging buffer can be released.
    this.exports.tt_free(ptr, data.byteLength, data.byteLength);
    if (handle === 0) throw new WasmError(-1, this.lastError());
    return new FontHandle(this, handle);
  }
}

export class FontHandle {
  private closed = false;

  constructor(
    private readonly core: WasmCore,
    readonly handle: number,
  ) {}

  get numGlyphs(): number {
    return this.core.exports.tt_num_glyphs(this.handle);
  }

  get unitsPerEm(): number {
    return this.core.exports.tt_units_per_em(this.handle);
  }

  codepointToGid(cp: number): number {
    return this.core.exports.tt_codepoint_to_gid(this.handle, cp);
  }

  setPixelSize(ppem: number, hinting: boolean): void {
    this.core.check(
      this.core.exports.tt_set_pixel_size(this.handle, ppem, hinting ? 1 : 0),
    );
  }

  lineMetrics(): LineMetrics {
    const p = this.core.scratchPtr;
    this.core.check(this.core.exports.tt_line_metrics(this.handle, p));
    const v = new DataView(this.core.exports.memory.buffer);
    return {
      ascender: v.getFloat32(p, true),
      descender: v.getFloat32(p + 4, true),
      lineGap: v.getFloat32(p + 8, true),
    };
  }

  /**
   * Rasterize one glyph. The alpha plane is copied out of linear memory and
   * immediately freed inside the core, so callers own a plain Uint8Array.
   */
  renderGlyph(gid: number): { alpha: Uint8Array; metrics: GlyphResult } {
    const p = this.core.scratchPtr;
    this.core.check(this.core.exports.tt_render_glyph(this.handle, gid, p));
    const v = new DataView(this.core.exports.memory.buffer);
    const result: GlyphResult = {
      width: v.getUint32(p, true),
      height: v.getUint32(p + 4, true),
      left: v.getInt32(p + 8, true),
      top: v.getInt32(p + 12, true),
      advance: v.getFloat32(p + 16, true),
      lsb: v.getFloat32(p + 20, true),
      alphaPtr: v.getUint32(p + 24, true),
      alphaLen: v.getUint32(p + 28, true),
    };
    let alpha: Uint8Array;
    if (result.alphaPtr !== 0 && result.alphaLen > 0) {
      alpha = new Uint8Array(
        this.core.exports.memory.buffer,
        result.alphaPtr,
        result.alphaLen,
      ).slice();
      this.core.exports.tt_alpha_free(result.alphaPtr, result.alphaLen);
    } else {
      alpha = new Uint8Array(0);
    }
    return { alpha, metrics: result };
  }

  close(): void {
    if (this.closed) return;
    this.core.exports.tt_font_close(this.handle);
    this.closed = true;
  }
}
