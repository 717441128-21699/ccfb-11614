/**
 * UI controller: loads the WASM core and a user-supplied TTF/TTC, registers
 * the same font bytes with the platform's FontFace for a fair side-by-side
 * comparison, renders both pipelines, measures per-glyph rasterization time,
 * and runs the pixel diff.
 */

import { WasmCore, FontHandle } from './wasm-core';
import { GlyphCache, TextLayoutEngine } from './glyph-cache';
import {
  renderSystemText,
  diffPlanes,
  composeOurPlane,
  type RenderedPlane,
} from './system-render';

const $ = <T extends HTMLElement>(id: string): T =>
  document.getElementById(id) as T;

class App {
  private core!: WasmCore;
  private font: FontHandle | null = null;
  private cache: GlyphCache | null = null;
  private layoutEngine: TextLayoutEngine | null = null;
  private zoom = 4;
  private lastOurs: { gray: Uint8ClampedArray; width: number; height: number } | null = null;
  private lastSystem: RenderedPlane | null = null;

  async init(): Promise<void> {
    this.core = await WasmCore.instantiate(fetch('/core/ttfont_core.wasm'));
    this.bindEvents();
    this.setStatus('WASM 内核已就绪，请选择字体文件');
  }

  private bindEvents(): void {
    $('font-file').addEventListener('change', (e) => this.onFontFile(e));
    $('run').addEventListener('click', () => void this.run());
    $('zoom').addEventListener('input', (e) => {
      this.zoom = Number((e.target as HTMLInputElement).value);
      $('zoom-label').textContent = `${this.zoom}×`;
      this.redrawCanvases();
    });
    document.querySelectorAll<HTMLButtonElement>('.presets button').forEach((btn) => {
      btn.addEventListener('click', () => {
        $<HTMLInputElement>('text-input').value = btn.dataset.text ?? '';
      });
    });
  }

  private setStatus(msg: string): void {
    $('font-name').textContent = msg;
  }

  private async onFontFile(e: Event): Promise<void> {
    const input = e.target as HTMLInputElement;
    const file = input.files?.[0];
    if (!file) return;
    const buffer = await file.arrayBuffer();
    const bytes = new Uint8Array(buffer);

    this.font?.close();
    try {
      this.font = this.core.openFont(bytes, 0);
      this.cache = new GlyphCache(this.font);
      this.layoutEngine = new TextLayoutEngine(this.cache);
      this.setStatus(
        `${file.name} · ${this.font.numGlyphs} glyphs · UPM ${this.font.unitsPerEm}`,
      );
      // Register the same bytes with the platform stack for the reference side.
      const face = new FontFace('system-ref', buffer.slice(0), { style: 'normal' });
      await face.load();
      document.fonts.add(face);
      $<HTMLInputElement>('system-family').value = 'system-ref';
    } catch (err) {
      this.setStatus(`加载失败: ${(err as Error).message}`);
      this.font = null;
    }
  }

  private async run(): Promise<void> {
    if (!this.font || !this.cache || !this.layoutEngine) {
      this.setStatus('请先选择字体文件');
      return;
    }
    const ppem = Number($<HTMLInputElement>('ppem').value);
    const hinting = $<HTMLInputElement>('hinting').checked;
    const text = $<HTMLInputElement>('text-input').value;
    const family = $<HTMLInputElement>('system-family').value;
    this.font.setPixelSize(ppem, hinting);
    this.cache.clear();

    // Cold-cache timing: measure only the WASM rasterization path.
    const t0 = performance.now();
    const layout = this.layoutEngine.layoutLine(text, ppem, hinting, this.font);
    const t1 = performance.now();
    const rasterizedCount = layout.glyphs.length;
    const perGlyphMs = rasterizedCount > 0 ? (t1 - t0) / rasterizedCount : 0;

    // System reference at the identical ppem and text.
    const system = renderSystemText({ fontFamily: family, ppem, text });
    const oursGray = composeOurPlane(
      layout.glyphs.map((g) => ({
        glyph: g.glyph,
        x: g.x,
        baselineY: system.baselineY,
      })),
      system.width,
      system.height,
      system.baselineY,
    );
    this.lastOurs = { gray: oursGray, width: system.width, height: system.height };
    this.lastSystem = system;

    const diff = diffPlanes(oursGray, system.gray, system.width, system.height, {
      grayscaleThreshold: 32,
    });

    this.updateMetrics(perGlyphMs, rasterizedCount, diff);
    this.redrawCanvases();
  }

  private updateMetrics(
    perGlyphMs: number,
    count: number,
    diff: ReturnType<typeof diffPlanes>,
  ): void {
    const stats = this.cache?.stats;
    $('m-hits').textContent = stats ? `${stats.hits}/${stats.hits + stats.misses}` : '–';
    $('m-count').textContent = String(count);
    $('m-time').textContent = `${perGlyphMs.toFixed(3)} ms`;
    $('m-diff').textContent =
      `${diff.differingPixels}/${diff.totalPixels} ` +
      `(${(diff.differenceRatio * 100).toFixed(2)}%)`;
    $('m-mae').textContent = diff.meanAbsError.toFixed(2);
    const verdict = $('m-verdict');
    const withinBudget = perGlyphMs <= 0.2;
    const withinDiff = diff.differenceRatio <= 0.05;
    verdict.classList.remove('pass', 'fail');
    if (withinDiff && withinBudget) {
      verdict.textContent = '✓ 达标 (≤5% 且 ≤0.2ms/字)';
      verdict.classList.add('pass');
    } else {
      const reasons = [
        withinDiff ? null : '差异>5%',
        withinBudget ? null : '单字>0.2ms',
      ]
        .filter(Boolean)
        .join('，');
      verdict.textContent = `✗ 未达标 (${reasons})`;
      verdict.classList.add('fail');
    }
  }

  private redrawCanvases(): void {
    if (!this.lastOurs || !this.lastSystem) return;
    const z = this.zoom;
    const { width, height, gray } = this.lastOurs;
    const oursCanvas = $('ours-canvas') as HTMLCanvasElement;
    drawGray(oursCanvas, gray, width, height, z);

    const sysCanvas = $('system-canvas') as HTMLCanvasElement;
    drawImageScaled(sysCanvas, this.lastSystem.canvas, z);

    const diffCanvas = $('diff-canvas') as HTMLCanvasElement;
    const diff = diffPlanes(
      this.lastOurs.gray,
      this.lastSystem.gray,
      width,
      height,
      { grayscaleThreshold: 32 },
    );
    drawImageScaled(diffCanvas, diff.heatmap, z);
  }
}

function drawGray(
  canvas: HTMLCanvasElement,
  gray: Uint8ClampedArray,
  width: number,
  height: number,
  zoom: number,
): void {
  canvas.width = width * zoom;
  canvas.height = height * zoom;
  const ctx = canvas.getContext('2d')!;
  const img = ctx.createImageData(width, height);
  for (let i = 0; i < gray.length; i++) {
    const v = gray[i];
    img.data[i * 4] = v;
    img.data[i * 4 + 1] = v;
    img.data[i * 4 + 2] = v;
    img.data[i * 4 + 3] = 255;
  }
  // Put at native size then scale up via a temporary canvas for crisp pixels.
  const tmp = document.createElement('canvas');
  tmp.width = width;
  tmp.height = height;
  tmp.getContext('2d')!.putImageData(img, 0, 0);
  ctx.imageSmoothingEnabled = false;
  ctx.drawImage(tmp, 0, 0, canvas.width, canvas.height);
}

function drawImageScaled(
  canvas: HTMLCanvasElement,
  source: HTMLCanvasElement,
  zoom: number,
): void {
  canvas.width = source.width * zoom;
  canvas.height = source.height * zoom;
  const ctx = canvas.getContext('2d')!;
  ctx.imageSmoothingEnabled = false;
  ctx.drawImage(source, 0, 0, canvas.width, canvas.height);
}

const app = new App();
void app.init();
