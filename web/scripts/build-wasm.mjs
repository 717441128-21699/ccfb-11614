// 调用 cargo 把 core 编译成 wasm,再拷贝到 web 的静态资源目录。
// 不依赖 wasm-pack/wasm-bindgen,产物是纯 C ABI wasm,由 loader.ts 手工装载。
import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync, existsSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '..');
const coreDir = resolve(root, '..', 'core');
const wasmSrc = resolve(
  coreDir,
  'target',
  'wasm32-unknown-unknown',
  'release',
  'ttengine.wasm',
);
const outDir = resolve(root, 'wasm');
const wasmDst = resolve(outDir, 'ttengine.wasm');

console.log('[build:wasm] cargo build --target wasm32-unknown-unknown --release');
execFileSync(
  'cargo',
  ['build', '--release', '--target', 'wasm32-unknown-unknown'],
  { cwd: coreDir, stdio: 'inherit' },
);

if (!existsSync(wasmSrc)) {
  throw new Error(`WASM 产物不存在: ${wasmSrc}`);
}
mkdirSync(outDir, { recursive: true });
copyFileSync(wasmSrc, wasmDst);
const kb = (existsSync(wasmDst) ? 0 : 0); // 占位,实际大小在下面打印
const { size } = await import('node:fs').then((fs) =>
  fs.promises.stat(wasmDst),
);
console.log(`[build:wasm] 已输出 ${wasmDst} (${(size / 1024).toFixed(1)} KB)`);
void kb;
