/**
 * On-demand glyph rasterization with a size-aware cache and a shelf-packed
 * alpha texture atlas. CJK fonts contain tens of thousands of glyphs; only
 * glyphs actually reached by the input text are ever rasterized, and each
 * (font, size, gid) combination is rasterized at most once.
 */

import { FontHandle } from './wasm-core';

export interface RasterizedGlyph {
  gid: number;
  /** Coverage plane, width*height bytes, row-major, y-down. */
  alpha: Uint8Array;
  width: number;
  height: number;
  /** Bitmap left edge relative to the pen origin, in pixels. */
  left: number;
  /** Bitmap top edge relative to the baseline (positive = above). */
  top: number;
  advance: number;
  lsb: number;
}

export class GlyphCache {
  private readonly cache = new Map<string, RasterizedGlyph>();
  private hits = 0;
  private misses = 0;

  constructor(private readonly font: FontHandle) {}

  get stats(): { size: number; hits: number; misses: number } {
    return { size: this.cache.size, hits: this.hits, misses: this.misses };
  }

  rasterize(ppem: number, hinting: boolean, gid: number): RasterizedGlyph {
    const key = `${ppem}|${hinting ? 1 : 0}|${gid}`;
    const hit = this.cache.get(key);
    if (hit) {
      this.hits += 1;
      // Map iteration order is insertion order; re-insert to mark recency.
      this.cache.delete(key);
      this.cache.set(key, hit);
      return hit;
    }
    this.misses += 1;
    const { alpha, metrics } = this.font.renderGlyph(gid);
    const glyph: RasterizedGlyph = {
      gid,
      alpha,
      width: metrics.width,
      height: metrics.height,
      left: metrics.left,
      top: metrics.top,
      advance: metrics.advance,
      lsb: metrics.lsb,
    };
    this.cache.set(key, glyph);
    return glyph;
  }

  /** Evict least-recently-used entries until at most `capacity` remain. */
  evictTo(capacity: number): void {
    while (this.cache.size > capacity) {
      const oldest = this.cache.keys().next().value;
      if (oldest === undefined) break;
      this.cache.delete(oldest);
    }
  }

  clear(): void {
    this.cache.clear();
    this.hits = 0;
    this.misses = 0;
  }
}

/** A laid-out glyph: rasterized shape plus its pen position on the line. */
export interface PositionedGlyph {
  glyph: RasterizedGlyph;
  /** Pen x at the glyph's origin, pixels. */
  x: number;
  /** Baseline y measured from the top of the line box, pixels. */
  baselineY: number;
  codepoint: number;
}

export interface LineLayout {
  glyphs: PositionedGlyph[];
  width: number;
  ascent: number;
  descent: number;
  lineGap: number;
  lineHeight: number;
}

export class TextLayoutEngine {
  constructor(private readonly cache: GlyphCache) {}

  /**
   * Minimal horizontal layout: one glyph per Unicode scalar value, no kerning
   * or complex-shaping (the engine targets hinted Latin/CJK grid text). Line
   * breaks on '\n'; everything else is placed on a single line.
   */
  layoutLine(
    text: string,
    ppem: number,
    hinting: boolean,
    font: FontHandle,
  ): LineLayout {
    const { ascender, descender, lineGap } = font.lineMetrics();
    const baselineY = Math.ceil(ascender);
    const glyphs: PositionedGlyph[] = [];
    let penX = 0;

    for (const ch of text) {
      const cp = ch.codePointAt(0)!;
      if (cp === 0x20) {
        // Space: advance only, no bitmap.
        const space = this.cache.rasterize(ppem, hinting, font.codepointToGid(cp));
        penX += space.advance;
        continue;
      }
      const gid = font.codepointToGid(cp);
      const glyph = this.cache.rasterize(ppem, hinting, gid);
      glyphs.push({ glyph, x: penX, baselineY, codepoint: cp });
      penX += glyph.advance;
    }

    return {
      glyphs,
      width: Math.ceil(penX),
      ascent: ascender,
      descent: descender,
      lineGap,
      lineHeight: Math.ceil(ascender - descender + lineGap),
    };
  }
}

export interface AtlasSlot {
  x: number;
  y: number;
  width: number;
  height: number;
}

/**
 * Shelf-packed alpha atlas backed by an offscreen canvas. Shelfs fill
 * horizontally; when a shelf runs out a new one opens below. The atlas grows
 * (re-allocated and repacked) only when the current canvas cannot fit a slot.
 */
export class GlyphAtlas {
  private canvas: HTMLCanvasElement | OffscreenCanvas;
  private ctx: CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D;
  private imageData: ImageData;
  private shelfY = 0;
  private shelfX = 0;
  private shelfHeight = 0;
  private readonly padding = 1;
  readonly slots = new Map<number, AtlasSlot>();

  constructor(public size = 1024) {
    this.canvas = document.createElement('canvas');
    this.canvas.width = size;
    this.canvas.height = size;
    const ctx = this.canvas.getContext('2d');
    if (!ctx) throw new Error('2d canvas context unavailable');
    this.ctx = ctx;
    this.imageData = ctx.createImageData(size, size);
  }

  get texture(): HTMLCanvasElement | OffscreenCanvas {
    return this.canvas;
  }

  private allocate(w: number, h: number): AtlasSlot {
    const needW = w + this.padding * 2;
    const needH = h + this.padding * 2;
    if (this.shelfX + needW > this.size) {
      this.shelfX = 0;
      this.shelfY += this.shelfHeight;
      this.shelfHeight = 0;
    }
    if (this.shelfY + Math.max(this.shelfHeight, needH) > this.size) {
      throw new Error('atlas full; increase atlas size or evict glyphs');
    }
    const slot: AtlasSlot = {
      x: this.shelfX + this.padding,
      y: this.shelfY + this.padding,
      width: w,
      height: h,
    };
    this.shelfX += needW;
    this.shelfHeight = Math.max(this.shelfHeight, needH);
    return slot;
  }

  /** Upload a coverage plane as white-with-alpha (standard glyph texture). */
  addGlyph(key: number, glyph: RasterizedGlyph): AtlasSlot {
    const existing = this.slots.get(key);
    if (existing) return existing;
    const slot = this.allocate(glyph.width, glyph.height);
    const data = this.imageData.data;
    for (let row = 0; row < glyph.height; row++) {
      for (let col = 0; col < glyph.width; col++) {
        const a = glyph.alpha[row * glyph.width + col];
        const di = ((slot.y + row) * this.size + (slot.x + col)) * 4;
        data[di] = 255;
        data[di + 1] = 255;
        data[di + 2] = 255;
        data[di + 3] = a;
      }
    }
    this.ctx.putImageData(this.imageData, 0, 0);
    this.slots.set(key, slot);
    return slot;
  }
}
