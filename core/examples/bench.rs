//! 性能基准:测量栅格化单个字形的端到端耗时(解析命中缓存后的重复渲染 + 冷渲染)。
//!
//! 运行:cargo run --release --example bench [字体路径] [ppem]

use std::time::Instant;
use ttengine::engine::{Engine, Face};

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "C:/Windows/Fonts/simhei.ttf".to_string());
    let ppem: u16 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(24);

    let data = std::fs::read(&path).expect("read font");
    let face = Face::parse(&data, 0).expect("parse face");
    let mut engine = Engine::new(face, ppem);

    // 一组常用汉字(覆盖不同笔画复杂度)
    let sample: Vec<char> = "永的一是不了人我在有中国大小天地山水火木日月经天江河行地风云雨雪春夏秋冬上下左右前后东西南北甲乙丙丁子丑寅卯金木水火土乾坤坎离".chars().collect();
    let gids: Vec<u16> = sample
        .iter()
        .map(|&c| engine.glyph_id(c as u32).unwrap())
        .collect();

    // 预热(触发首次解析/微调/栅格化)
    for &g in &gids {
        let _ = engine.render_glyph(g).unwrap();
    }

    // 冷渲染:每个字形都重新走完整管线(模拟按需首次加载)
    let iterations = 200usize;
    let mut cold_times = Vec::with_capacity(iterations * gids.len());
    for _ in 0..iterations {
        for &g in &gids {
            let t0 = Instant::now();
            let r = engine.render_glyph(g).unwrap();
            let dt = t0.elapsed().as_secs_f64() * 1000.0;
            std::hint::black_box(&r);
            cold_times.push(dt);
        }
    }

    let n = cold_times.len();
    let mut sorted = cold_times.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = cold_times.iter().sum::<f64>() / n as f64;
    let p50 = sorted[n / 2];
    let p95 = sorted[(n as f64 * 0.95) as usize];
    let p99 = sorted[(n as f64 * 0.99) as usize];
    let max = sorted[n - 1];

    println!("字体: {}", path);
    println!("字号: {}px,样本字形数: {},每字形测量次数: {}", ppem, gids.len(), iterations);
    println!("总测量: {} 次", n);
    println!("--- 单字形端到端耗时(ms) ---");
    println!("mean = {:.4}", mean);
    println!("p50  = {:.4}", p50);
    println!("p95  = {:.4}", p95);
    println!("p99  = {:.4}", p99);
    println!("max  = {:.4}", max);
    println!();
    let budget = 0.2;
    println!(
        "指标 < 0.2ms:p50 {} / p95 {} / max {}",
        if p50 < budget { "达标" } else { "未达标" },
        if p95 < budget { "达标" } else { "未达标" },
        if max < budget { "达标" } else { "未达标" }
    );

    // 微调开销单独测量
    engine.hinting_enabled = true;
    let t0 = Instant::now();
    let mut hint_count = 0u32;
    for &g in &gids {
        let _ = engine.render_glyph(g);
        hint_count += 1;
    }
    let hinted_ms = t0.elapsed().as_secs_f64() * 1000.0;
    engine.hinting_enabled = false;
    let t0 = Instant::now();
    for &g in &gids {
        let _ = engine.render_glyph(g);
    }
    let unhinted_ms = t0.elapsed().as_secs_f64() * 1000.0;
    println!();
    println!("--- 微调开关对比(整组 {} 字形, ms) ---", hint_count);
    println!("hinted   = {:.3} ({:.4}/字形)", hinted_ms, hinted_ms / gids.len() as f64);
    println!("unhinted = {:.3} ({:.4}/字形)", unhinted_ms, unhinted_ms / gids.len() as f64);

    let stats = engine.hint_stats();
    println!();
    println!("--- 微调 VM 统计 ---");
    println!("opcodes_run={} unknown={} aborts={}", stats.opcodes_run, stats.unknown_opcodes, stats.aborts);
}
