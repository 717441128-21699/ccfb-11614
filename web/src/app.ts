/**
 * 应用主控:装载字体 → 初始化 WASM 引擎 → 左右并排渲染 → 逐像素对比。
 */
import { TtEngine, type EntryMetrics } from './wasm/loader.js';
import { AtlasRenderer, type PlacedGlyph } from './render/atlas-renderer.js';
import { drawSystemLine, readCanvas, registerFontFace } from './render/system-renderer.js';
import { compareAlpha, glCanvasToImageData, DEFAULT_DIFF } from './render/pixel-diff.js';

interface RenderedLine {
  glyphs: PlacedGlyph[];
  totalWidth: number;
}

export class App {
  private engine: TtEngine;
  private gl: AtlasRenderer;
  private canvasOurs: HTMLCanvasElement;
  private canvasSystem: HTMLCanvasElement;
  private canvasDiff: HTMLCanvasElement;
  private fontBytes: Uint8Array | null = null;
  private family = 'ttengine-system-ref';
  private ppem = 24;

  private constructor(engine: TtEngine) {
    this.engine = engine;
    this.canvasOurs = document.getElementById('canvas-ours') as HTMLCanvasElement;
    this.canvasSystem = document.getElementById('canvas-system') as HTMLCanvasElement;
    this.canvasDiff = document.getElementById('canvas-diff') as HTMLCanvasElement;
    this.gl = new AtlasRenderer(this.canvasOurs);
  }

  static async create(wasmUrl: string): Promise<App> {
    const engine = await TtEngine.load(wasmUrl);
    return new App(engine);
  }

  /** 处理字体文件:写引擎 + 注册系统 FontFace。 */
  async loadFont(file: File, ppem: number): Promise<void> {
    const buf = new Uint8Array(await file.arrayBuffer());
    this.fontBytes = buf;
    this.ppem = ppem;
    const ATLAS = 1024;
    this.engine.initFont(buf, 0, ppem, ATLAS, ATLAS);
    // 同一份字节注册给系统,保证对比公平
    const base = file.name.replace(/\.[^.]+$/, '').replace(/[^a-zA-Z0-9]/g, '') || 'ttfont';
    this.family = `ttref-${base}`;
    await registerFontFace(buf, this.family);
    this.engine.clearAtlas();
  }

  setPpem(ppem: number): void {
    if (!this.fontBytes || ppem === this.ppem) return;
    this.ppem = ppem;
    this.engine.setPpem(ppem);
    this.engine.clearAtlas();
  }

  /** 用自研 WASM 引擎渲染一行,返回四边形列表。 */
  private renderOurs(text: string): RenderedLine {
    const line = this.engine.lineMetrics();
    const baseline = Math.ceil(line.ascent) + 4;
    let pen = 4;
    const glyphs: PlacedGlyph[] = [];
    for (const ch of text) {
      const gid = this.engine.glyphId(ch.codePointAt(0)!);
      const m: EntryMetrics = this.engine.render(gid);
      if (m.width > 0 && m.height > 0) {
        glyphs.push({
          sx: m.x, sy: m.y, sw: m.width, sh: m.height,
          // topBearing 是基线上方高度,换算成画布 y
          dx: pen + Math.round(m.leftSideBearing),
          dy: baseline - Math.round(m.topBearing),
          dw: m.width,
          dh: m.height,
        });
      }
      pen += Math.round(m.advance);
    }
    return { glyphs, totalWidth: pen + 4 };
  }

  /** 完整渲染并对比。 */
  async run(text: string): Promise<{ ratio: number; mean: number; stats: ReturnType<TtEngine['hintStats']> }> {
    if (!this.fontBytes) throw new Error('尚未装载字体');
    const line = this.engine.lineMetrics();
    const lineH = Math.ceil(line.ascent - line.descent + line.lineGap) + 8;
    const W = Math.max(640, Math.ceil(text.length * this.ppem * 1.4) + 24);
    const H = Math.max(96, lineH + 16);

    // —— 左侧:自研引擎 ——
    this.canvasOurs.width = W;
    this.canvasOurs.height = H;
    const rendered = this.renderOurs(text);
    const atlas = this.engine.atlasPixels();
    this.gl.uploadAtlas(atlas.data, atlas.width, atlas.height);
    this.gl.draw(rendered.glyphs, [0.07, 0.07, 0.09, 1], W, H);

    // —— 右侧:系统渲染 ——
    this.canvasSystem.width = W;
    this.canvasSystem.height = H;
    const sctx = this.canvasSystem.getContext('2d')!;
    sctx.clearRect(0, 0, W, H);
    const baseline = Math.ceil(line.ascent) + 4;
    drawSystemLine(sctx, text, this.family, this.ppem, 4, baseline);

    // —— 逐像素对比 ——
    const oursImg = glCanvasToImageData(this.canvasOurs);
    const sysImg = readCanvas(sctx, W, H);
    const result = compareAlpha(oursImg, sysImg, DEFAULT_DIFF);

    this.canvasDiff.width = result.width;
    this.canvasDiff.height = result.height;
    const dctx = this.canvasDiff.getContext('2d')!;
    dctx.putImageData(result.heatmap, 0, 0);

    return {
      ratio: result.ratio,
      mean: result.meanAbsDiff,
      stats: this.engine.hintStats(),
    };
  }
}
