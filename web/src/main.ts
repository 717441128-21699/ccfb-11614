import { App } from './app.js';

const $ = <T extends HTMLElement>(id: string): T => document.getElementById(id) as T;

async function main(): Promise<void> {
  const status = $<HTMLElement>('status');
  const setStatus = (msg: string, ok = true): void => {
    status.textContent = msg;
    status.className = ok ? 'status ok' : 'status err';
  };

  let app: App;
  try {
    app = await App.create(new URL('../wasm/ttengine.wasm', import.meta.url).toString());
    setStatus('WASM 引擎已就绪,请选择 TrueType 字体文件');
  } catch (e) {
    setStatus(`WASM 加载失败: ${(e as Error).message}`, false);
    return;
  }

  let loaded = false;
  const fontInput = $<HTMLInputElement>('font-file');
  const ppemInput = $<HTMLInputElement>('ppem');
  const textInput = $<HTMLTextAreaElement>('text-input');
  const runBtn = $<HTMLButtonElement>('run-btn');
  const resultBox = $<HTMLElement>('diff-result');
  const statsBox = $<HTMLElement>('hint-stats');

  async function ensureLoaded(): Promise<boolean> {
    const file = fontInput.files?.[0];
    if (!file) {
      setStatus('请先选择 .ttf / .ttc 字体文件', false);
      return false;
    }
    const ppem = Number(ppemInput.value);
    try {
      await app.loadFont(file, ppem);
      loaded = true;
      setStatus(`已装载 ${file.name} @ ${ppem}px`);
      return true;
    } catch (e) {
      setStatus(`字体装载失败: ${(e as Error).message}`, false);
      return false;
    }
  }

  fontInput.addEventListener('change', () => {
    void ensureLoaded();
  });
  ppemInput.addEventListener('change', () => {
    if (loaded) app.setPpem(Number(ppemInput.value));
  });

  runBtn.addEventListener('click', async () => {
    if (!loaded) {
      const ok = await ensureLoaded();
      if (!ok) return;
    }
    const text = textInput.value || '永的一是中国';
    const t0 = performance.now();
    try {
      const { ratio, mean, stats } = await app.run(text);
      const ms = (performance.now() - t0).toFixed(2);
      const pct = (ratio * 100).toFixed(2);
      const pass = ratio <= 0.05;
      resultBox.innerHTML = `
        <div class="metric ${pass ? 'good' : 'warn'}">
          <span class="num">${pct}%</span>
          <span class="label">差异像素占比(阈值 5%)</span>
        </div>
        <div class="metric">
          <span class="num">${mean.toFixed(1)}</span>
          <span class="label">平均 alpha 差(0-255)</span>
        </div>
        <div class="metric">
          <span class="num">${ms} ms</span>
          <span class="label">整行渲染+对比耗时</span>
        </div>`;
      statsBox.textContent =
        `微调 VM:已执行 ${stats.opcodes.toLocaleString()} 条指令,` +
        `未实现 ${stats.unknown} 个,硬中止 ${stats.aborts} 次`;
      setStatus(pass ? '对比完成:相似度达标(≤5%)' : '对比完成:差异超过 5%,见热力图', pass);
    } catch (e) {
      setStatus(`渲染失败: ${(e as Error).message}`, false);
    }
  });
}

void main();
