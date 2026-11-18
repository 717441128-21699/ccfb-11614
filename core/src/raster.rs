//! 扫描线解析覆盖率栅格化器。
//!
//! 思路(stb_truetype 同源,独立实现):
//! 1. 二次贝塞尔按"控制点到弦距离"自适应展平为线段,容差约 1/4 子像素;
//! 2. 像素行方向做 `SS=4` 倍垂直超采样,每条子扫描线求所有相交边;
//! 3. 每条边在一个像素内贡献 *有符号分数覆盖率* = 跨度比例 × 方向,
//!    按非零环绕(non-zero winding)累加后截断到 [0,1];
//! 4. 4 条子扫描线平均 -> 8-bit alpha。
//!
//! 坐标全部使用 26.6 定点,与微调解释器输出无缝衔接。

use crate::fixed::F26Dot6;
use crate::glyf::{Point, SimpleGlyph};

/// 垂直超采样倍数。
/// 8 倍垂直采样显著降低 12px 小字号细横笔画的漏采样(笔画落在扫描线
/// 之间而消失的问题);性能仍远低于 0.2ms 预算。
pub const SS: i32 = 8;

#[derive(Clone, Copy, Debug)]
struct Edge {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    /// 下行边(y 增大方向)为 +1,上行为 -1
    sign: i32,
}

/// 栅格化输出:8-bit 覆盖率位图,左上角 `(left, top)` 为画布像素坐标。
#[derive(Clone, Debug)]
pub struct Bitmap {
    pub width: i32,
    pub height: i32,
    pub left: i32,
    pub top: i32,
    pub alpha: Vec<u8>,
}

impl Bitmap {
    #[inline]
    pub fn idx(&self, x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            None
        } else {
            Some((y * self.width + x) as usize)
        }
    }
}

/// 栅格化参数。点坐标已是 26.6 像素值(相对画布原点)。
pub fn rasterize(points: &[Point], end_pts: &[u16], width: i32, height: i32) -> Bitmap {
    let n = (width * height) as usize;
    // wind 存"缠绕数增量":每条边只改两个相邻像素,最后逐行前缀和。
    let mut wind = vec![0f32; n];
    if points.is_empty() || width <= 0 || height <= 0 {
        return Bitmap {
            width,
            height,
            left: 0,
            top: 0,
            alpha: vec![0u8; n],
        };
    }

    let mut edges: Vec<Edge> = Vec::with_capacity(points.len() * 2);
    flatten_contours(points, end_pts, &mut edges);
    if edges.is_empty() {
        return Bitmap {
            width,
            height,
            left: 0,
            top: 0,
            alpha: vec![0u8; n],
        };
    }

    // 子像素高度(浮点像素)
    let sub_h = 1.0 / SS as f32;

    for row in 0..height {
        for s in 0..SS {
            // 子扫描线 y 位置(像素浮点)
            let sy = row as f32 + (s as f32 + 0.5) * sub_h;
            for e in &edges {
                // 半开区间 [y0, y1),顶点不重复计数
                if sy < e.y0 || sy >= e.y1 {
                    continue;
                }
                let t = (sy - e.y0) / (e.y1 - e.y0);
                let x = e.x0 + (e.x1 - e.x0) * t;
                if x < 0.0 || x >= width as f32 {
                    continue;
                }
                let px = x as i32;
                let frac = x - px as f32;
                // stb 风格:边在像素 px 内留下 sign*(1-frac) 的缠绕增量,
                // 跨过像素边界的 frac 部分记到右侧邻居(符号相反)。
                wind[(row * width + px) as usize] += e.sign as f32 * (1.0 - frac);
                if (px + 1) < width {
                    wind[(row * width + px + 1) as usize] -= e.sign as f32 * frac;
                }
            }
        }
    }

    // 逐行前缀和 -> 实际缠绕数;非零环绕填充取绝对值后截断到 [0,1]。
    let mut alpha = Vec::with_capacity(n);
    for row in 0..height {
        let mut cover = 0f32;
        for col in 0..width {
            cover += wind[(row * width + col) as usize];
            let a = (cover.abs() / SS as f32).min(1.0);
            alpha.push((a * 255.0 + 0.5) as u8);
        }
    }
    Bitmap {
        width,
        height,
        left: 0,
        top: 0,
        alpha,
    }
}

/// 把带二次贝塞尔的轮廓展平为有向边。
///
/// 两阶段:先把每条轮廓归约成一串 on-curve 多边形顶点
/// (直线点原样保留,贝塞尔递归细分插入中间点),
/// 再在相邻顶点之间(含末点回起点的闭合边)统一连边。
/// 这样直线段与曲线段都不会漏边。
fn flatten_contours(points: &[Point], end_pts: &[u16], edges: &mut Vec<Edge>) {
    let mut start = 0usize;
    for &end in end_pts {
        let end = end as usize;
        if end < start || end >= points.len() {
            break;
        }
        let n = end - start + 1;
        if n < 2 {
            start = end + 1;
            continue;
        }
        let contour = &points[start..=end];
        let last_on = contour[n - 1].on_curve;

        // 1) 归约出 on-curve 多边形顶点(不含重复的闭合起点)
        let mut poly: Vec<Point> = Vec::with_capacity(contour.len() * 2);
        if !contour[0].on_curve {
            let seed = if last_on {
                contour[n - 1]
            } else {
                midpoint(contour[n - 1], contour[0])
            };
            poly.push(seed);
        }

        let mut i = 0;
        while i < n {
            let p = contour[i];
            if p.on_curve {
                poly.push(p);
                i += 1;
            } else {
                let next = if i + 1 < n {
                    contour[i + 1]
                } else if last_on {
                    contour[n - 1]
                } else {
                    midpoint(contour[n - 1], contour[0])
                };
                let end_pt = if next.on_curve {
                    next
                } else {
                    midpoint(p, next)
                };
                let from = *poly.last().unwrap();
                collect_subdivision(from, p, end_pt, 0, &mut poly);
                poly.push(end_pt);
                i += if next.on_curve { 2 } else { 1 };
            }
        }

        // 2) 相邻顶点统一连边 + 闭合
        if poly.len() >= 2 {
            for w in poly.windows(2) {
                push_edge(edges, w[0], w[1]);
            }
            push_edge(edges, *poly.last().unwrap(), poly[0]);
        }
        start = end + 1;
    }
}

#[inline]
fn midpoint(a: Point, b: Point) -> Point {
    Point {
        x: (a.x + b.x) / 2,
        y: (a.y + b.y) / 2,
        on_curve: true,
    }
}

/// 二次贝塞尔递归细分,把内部点(不含 p0;逼近足够时不插入)收进 `out`。
fn collect_subdivision(p0: Point, pc: Point, p2: Point, depth: u32, out: &mut Vec<Point>) {
    // 容差:控制点到弦的距离阈值(26.6)
    const TOL2: i64 = 16;
    let dx = p2.x - p0.x;
    let dy = p2.y - p0.y;
    let cross = (pc.x - p0.x) as i64 * dy as i64 - (pc.y - p0.y) as i64 * dx as i64;
    let len2 = dx as i64 * dx as i64 + dy as i64 * dy as i64;
    let flat = depth >= 6 || len2 == 0 || cross * cross <= TOL2 * TOL2 * len2;
    if flat {
        return;
    }
    let p1 = midpoint(p0, pc);
    let p3 = midpoint(pc, p2);
    let pm = midpoint(p1, p3);
    collect_subdivision(p0, p1, pm, depth + 1, out);
    out.push(pm);
    collect_subdivision(pm, p3, p2, depth + 1, out);
}

#[inline]
fn push_edge(edges: &mut Vec<Edge>, a: Point, b: Point) {
    if a.y == b.y {
        return; // 水平边对扫描线覆盖率无贡献
    }
    let (p0, p1, sign) = if a.y < b.y {
        (a, b, 1i32)
    } else {
        (b, a, -1i32)
    };
    edges.push(Edge {
        x0: p0.x as f32 / 64.0,
        y0: p0.y as f32 / 64.0,
        x1: p1.x as f32 / 64.0,
        y1: p1.y as f32 / 64.0,
        sign,
    });
}

/// 把字体单位的简单字形缩放到 26.6 像素坐标。
pub fn scale_simple(g: &SimpleGlyph, scale: i32, shift_x: F26Dot6, shift_y: F26Dot6) -> Vec<Point> {
    g.points
        .iter()
        .map(|p| Point {
            x: F26Dot6::scale_font_units(p.x, scale).0 + shift_x.0,
            y: F26Dot6::scale_font_units(p.y, scale).0 + shift_y.0,
            on_curve: p.on_curve,
        })
        .collect()
}

/// PGM 灰度图导出,供测试肉眼检查。
pub fn write_pgm(path: &str, bm: &Bitmap) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    writeln!(f, "P5\n{} {}\n255", bm.width, bm.height)?;
    f.write_all(&bm.alpha)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sq(x0: i32, y0: i32, x1: i32, y1: i32) -> (Vec<Point>, Vec<u16>) {
        // 单位:26.6,顺时针(下行边在左侧)
        let pts = vec![
            Point { x: x0, y: y0, on_curve: true },
            Point { x: x1, y: y0, on_curve: true },
            Point { x: x1, y: y1, on_curve: true },
            Point { x: x0, y: y1, on_curve: true },
        ];
        (pts, vec![3])
    }

    #[test]
    fn filled_square_center_is_solid() {
        // 像素 1..4 的实心方块(64..256 in 26.6)
        let (pts, ends) = sq(64, 64, 256, 256);
        let bm = rasterize(&pts, &ends, 6, 6);
        // 中心像素应全不透明
        assert!(bm.alpha[(2 * 6 + 2) as usize] > 250);
        // 角落在轮廓外
        assert_eq!(bm.alpha[0], 0);
        assert_eq!(bm.alpha[(5 * 6 + 5) as usize], 0);
    }

    #[test]
    fn circle_has_aa_ring() {
        // 用四条二次贝塞尔拼一个圆(半径 2px,圆心 3,3)
        let c = 3 * 64;
        let r = 2 * 64;
        let k = (r as f32 * 0.5523) as i32;
        let pts = vec![
            Point { x: c, y: c - r, on_curve: true },
            Point { x: c + k, y: c - r, on_curve: false },
            Point { x: c + r, y: c - k, on_curve: false },
            Point { x: c + r, y: c, on_curve: true },
            Point { x: c + r, y: c + k, on_curve: false },
            Point { x: c + k, y: c + r, on_curve: false },
            Point { x: c, y: c + r, on_curve: true },
            Point { x: c - k, y: c + r, on_curve: false },
            Point { x: c - r, y: c + k, on_curve: false },
            Point { x: c - r, y: c, on_curve: true },
            Point { x: c - r, y: c - k, on_curve: false },
            Point { x: c - k, y: c - r, on_curve: false },
        ];
        let bm = rasterize(&pts, &[11], 7, 7);
        let center = bm.alpha[(3 * 7 + 3) as usize];
        // 圆心正对像素中心时,4 条子扫描线都落在圆内但逼近曲线边界,
        // 覆盖率允许略低于 1.0;实心判定阈值取 200。
        assert!(center > 200, "center should be near solid, got {center}");
        // 边缘存在中间调(抗锯齿生效)
        let has_gray = bm.alpha.iter().any(|&a| a > 20 && a < 235);
        assert!(has_gray, "expected AA gray pixels on circle edge");
        // 圆外四角必须为空
        assert_eq!(bm.alpha[0], 0);
        assert_eq!(bm.alpha[(6 * 7 + 6) as usize], 0);
    }

    #[test]
    fn render_arial_a_to_pgm() {
        use crate::font::FontFile;
        use crate::glyf::GlyfTable;
        use crate::tables::{Cmap, Head, Maxp};
        let data = std::fs::read("C:/Windows/Fonts/arial.ttf").unwrap();
        let f = FontFile::parse(&data, 0).unwrap();
        let head = Head::parse(&f).unwrap();
        let maxp = Maxp::parse(&f).unwrap();
        let glyf = GlyfTable::new(&f, head, maxp.num_glyphs).unwrap();
        let cmap = Cmap::parse(&f).unwrap();
        let gid = cmap.glyph_id('A' as u32).unwrap();
        let simple = glyf.flatten(gid).unwrap();
        let ppem = 32u16;
        let scale = crate::tables::pixel_scale(head.units_per_em, ppem);
        let pts = scale_simple(&simple, scale, F26Dot6(128), F26Dot6(128));
        let bm = rasterize(&pts, &simple.end_pts, 40, 40);
        std::fs::create_dir_all("test-out").ok();
        write_pgm("test-out/arial_a_32.pgm", &bm).unwrap();
        let covered = bm.alpha.iter().filter(|&&a| a > 10).count();
        assert!(covered > 50, "should cover many pixels, got {covered}");
    }
}
