#!/usr/bin/env node
// 一键无头端到端验证:启动检查 → headless Chrome 跑 headless.html →
// 解析 DOM 里的 HEADLESS_PASS/FAIL 标题与各项指标,非零退出码表示失败。
// 前置:已执行 npm run build(wasm + tsc),且 dev server 在 PORT 上运行。
import { execFileSync } from 'node:child_process';
import { writeFileSync, mkdtempSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const PORT = process.env.PORT || '8080';
const url = `http://localhost:${PORT}/headless.html`;
const candidates = [
  'C:/Program Files/Google/Chrome/Application/chrome.exe',
  'C:/Program Files (x86)/Google/Chrome/Application/chrome.exe',
  'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',
  'C:/Program Files/Microsoft/Edge/Application/msedge.exe',
];
const browser = candidates.find((p) => existsSync(p));
if (!browser) {
  console.error('未找到 Chrome/Edge,跳过无头验证');
  process.exit(2);
}

const profile = mkdtempSync(join(tmpdir(), 'tt-headless-'));
const domFile = join(tmpdir(), 'tt-dom.html');
const args = [
  '--headless=new',
  '--disable-gpu',
  '--no-sandbox',
  `--user-data-dir=${profile}`,
  '--virtual-time-budget=25000',
  '--run-all-compositor-stages-before-draw',
  '--dump-dom',
  url,
];
let dom = '';
// 虚拟时间预算偶发抖动导致异步流程未跑完,最多重试 3 次。
for (let attempt = 1; attempt <= 3; attempt++) {
  dom = execFileSync(browser, args, { encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 });
  const t = (dom.match(/<title>([^<]*)<\/title>/) || [])[1] || '';
  if (t.includes('PASS') || t.includes('FAIL')) break;
  console.error(`第 ${attempt} 次运行未取得结论,重试…`);
}
writeFileSync(domFile, dom);

const title = (dom.match(/<title>([^<]*)<\/title>/) || [])[1] || '';
const metrics = [...dom.matchAll(/\[(.*?)\]\s*([^\n<]+)/g)].map((m) => `${m[1]}: ${m[2].trim()}`);
const checks = [...dom.matchAll(/[✓✗]\s*([^\n<]+)/g)].map((m) => m[1].trim());

console.log('标题:', title);
console.log('\n指标:');
for (const m of metrics) console.log('  ' + m);
console.log('\n检查项:');
for (const c of checks) console.log('  - ' + c);

const failCount = (dom.match(/✗/g) || []).length;
if (title.includes('PASS') && failCount === 0) {
  console.log('\n无头端到端验证通过');
  process.exit(0);
} else {
  console.error('\n无头端到端验证失败');
  process.exit(1);
}
