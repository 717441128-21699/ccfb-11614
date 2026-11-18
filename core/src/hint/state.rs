//! 微调 VM 的图形状态与辅助原语。

use super::FunctionDef;
use super::HintStats;
use crate::fixed::{tt_round, F26Dot6};
use crate::glyf::{Point, SimpleGlyph};

/// 微调后的字形:点坐标为 26.6 像素值。
pub struct HintedGlyph {
    pub points: Vec<F26Dot6>,
    pub end_pts: Vec<u16>,
    /// 触碰标记:每个点两位 —— bit0 = x 触碰,bit1 = y 触碰
    pub touched: Vec<u8>,
    pub on_curve: Vec<bool>,
}

impl HintedGlyph {
    /// 字体单位 -> 26.6 像素的初始缩放(无微调)。
    pub fn from_simple(g: &SimpleGlyph, scale: i32) -> Self {
        let points = g
            .points
            .iter()
            .map(|p| F26Dot6::scale_font_units(p.x, scale))
            .chain(g.points.iter().map(|p| F26Dot6::scale_font_units(p.y, scale)))
            .collect();
        let on_curve = g.points.iter().map(|p| p.on_curve).collect();
        HintedGlyph {
            points,
            end_pts: g.end_pts.clone(),
            touched: vec![0u8; g.points.len()],
            on_curve,
        }
    }

    #[inline]
    pub fn num_points(&self) -> usize {
        self.touched.len()
    }

    #[inline]
    pub fn x(&self, i: usize) -> F26Dot6 {
        self.points[i]
    }
    #[inline]
    pub fn y(&self, i: usize) -> F26Dot6 {
        self.points[self.num_points() + i]
    }
    #[inline]
    pub fn set_x(&mut self, i: usize, v: F26Dot6) {
        self.points[i] = v;
    }
    #[inline]
    pub fn set_y(&mut self, i: usize, v: F26Dot6) {
        let n = self.num_points();
        self.points[n + i] = v;
    }

    #[inline]
    pub fn is_touched_x(&self, i: usize) -> bool {
        self.touched[i] & 1 != 0
    }
    #[inline]
    pub fn is_touched_y(&self, i: usize) -> bool {
        self.touched[i] & 2 != 0
    }
    #[inline]
    pub fn touch_x(&mut self, i: usize) {
        self.touched[i] |= 1;
    }
    #[inline]
    pub fn touch_y(&mut self, i: usize) {
        self.touched[i] |= 2;
    }
    #[inline]
    pub fn untouch_x(&mut self, i: usize) {
        self.touched[i] &= !1;
    }
    #[inline]
    pub fn untouch_y(&mut self, i: usize) {
        self.touched[i] &= !2;
    }

    /// 转回栅格化器需要的 [`Point`] 序列。
    pub fn to_raster_points(&self) -> Vec<Point> {
        let n = self.num_points();
        (0..n)
            .map(|i| Point {
                x: self.points[i].raw(),
                y: self.points[n + i].raw(),
                on_curve: self.on_curve[i],
            })
            .collect()
    }
}

/// 图形状态(graphics state)。
#[derive(Clone, Copy)]
pub struct GraphicsState {
    #[allow(dead_code)]
    pub auto_flip: bool,
    pub control_value_cut_in: F26Dot6,
    pub single_width_cut_in: F26Dot6,
    pub single_width_value: F26Dot6,
    pub delta_base: i32,
    pub delta_shift: i32,
    pub loop_count: i32,
    pub minimum_distance: F26Dot6,
    pub round_state: RoundState,
    pub rp: [usize; 2],
    pub gep: [F26Dot6; 2],
}

#[derive(Clone, Copy)]
pub struct RoundState {
    pub period: F26Dot6,
    pub phase: F26Dot6,
    pub threshold: F26Dot6,
}

impl Default for GraphicsState {
    fn default() -> Self {
        GraphicsState {
            auto_flip: true,
            control_value_cut_in: F26Dot6(17 * 64 / 16), // 17/16 px
            single_width_cut_in: F26Dot6::ZERO,
            single_width_value: F26Dot6::ZERO,
            delta_base: 9,
            delta_shift: 3,
            loop_count: 1,
            minimum_distance: F26Dot6::ONE,
            round_state: RoundState {
                period: F26Dot6::ONE,
                phase: F26Dot6::ZERO,
                threshold: F26Dot6::HALF,
            },
            rp: [0, 0],
            gep: [F26Dot6::ZERO, F26Dot6::ZERO],
        }
    }
}

impl GraphicsState {
    /// 规范 round():按当前 period/phase/threshold 舍入。
    #[inline]
    pub fn round(&self, distance: F26Dot6) -> F26Dot6 {
        tt_round(
            distance,
            self.round_state.period,
            self.round_state.phase,
            self.round_state.threshold,
        )
    }

    /// NROUND / ROUND / NO_ROUND 三种引擎距离处理。
    #[inline]
    #[allow(dead_code)]
    pub fn engine_distance(&self, distance: F26Dot6, do_round: bool) -> F26Dot6 {
        if do_round {
            self.round(distance)
        } else {
            distance
        }
    }

    /// ROUND/NROUND 四变体(操作码低 2 位):
    /// - bit1: 0=按 round_state 舍入(ROUND),1=不改变距离(NO_ROUND 语义)
    /// - bit0: 0=用 distance 本身,1=取负后再处理
    ///
    /// 返回处理后的引擎距离(26.6)。
    #[inline]
    pub fn engine_distance_variant(&self, distance: F26Dot6, variant: u8) -> F26Dot6 {
        let d = if variant & 1 != 0 {
            F26Dot6(-distance.0)
        } else {
            distance
        };
        if variant & 2 != 0 {
            // NROUND:不做舍入
            d
        } else {
            self.round(d)
        }
    }
}

/// 调用栈帧(FDEF / IDEF)。
pub struct CallFrame {
    pub return_addr: usize,
    pub saved_state: GraphicsState,
}

/// 微调虚拟机。借用字体级共享区与当前字形。
pub struct Vm<'a> {
    pub cvt: &'a mut [i32],
    pub storage: &'a mut [i32],
    pub fdefs: &'a mut [FunctionDef],
    pub idefs: &'a mut Vec<(u16, FunctionDef)>,
    pub stack: Vec<i32>,
    pub call_stack: Vec<CallFrame>,
    pub gs: GraphicsState,
    pub glyph: Option<&'a mut HintedGlyph>,
    /// 当前投影轴(true=x / false=y),初始 x(SVTCA[1] 语义)
    pub projection_is_x: bool,
    /// 当前自由向量轴(true=x / false=y)
    pub freedom_is_x: bool,
    pub scale: i32,
    pub ppem: u16,
    pub stats: &'a mut HintStats,
    /// 硬错误标志,置位后 run() 立即退出
    pub aborted: bool,
    /// 最近一条指令的程序计数器(诊断用)
    pub last_pc: usize,
    pub last_op: u8,
}

impl<'a> Vm<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cvt: &'a mut [i32],
        storage: &'a mut [i32],
        fdefs: &'a mut [FunctionDef],
        idefs: &'a mut Vec<(u16, FunctionDef)>,
        stack_cap: usize,
        _twilight_cap: usize,
        scale: i32,
        ppem: u16,
        stats: &'a mut HintStats,
    ) -> Self {
        Vm {
            cvt,
            storage,
            fdefs,
            idefs,
            stack: Vec::with_capacity(stack_cap),
            call_stack: Vec::with_capacity(32),
            gs: GraphicsState::default(),
            glyph: None,
            projection_is_x: true,
            freedom_is_x: true,
            scale,
            ppem,
            stats,
            aborted: false,
            last_pc: 0,
            last_op: 0,
        }
    }

    pub fn attach_glyph(&mut self, glyph: &'a mut HintedGlyph) {
        self.glyph = Some(glyph);
    }

    // ---- 栈原语(带边界保护,畸形字体不 panic) ----

    #[inline]
    pub fn push(&mut self, v: i32) {
        if self.stack.len() < self.stack.capacity() {
            self.stack.push(v);
        } else {
            self.abort("stack overflow");
        }
    }

    #[inline]
    pub fn pop(&mut self) -> i32 {
        match self.stack.pop() {
            Some(v) => v,
            None => {
                self.abort("stack underflow");
                0
            }
        }
    }

    /// 弹出一个点索引,并按 twilight 区语义归一化(负索引 = twilight,
    /// 当前实现把 twilight 退化为点 0,保证不越界)。
    #[inline]
    pub fn pop_point(&mut self) -> usize {
        let raw = self.pop();
        let n = self.glyph.as_ref().map(|g| g.num_points()).unwrap_or(1);
        if raw < 0 {
            // twilight 点:简化处理为 0 号点
            0
        } else {
            (raw as usize).min(n.saturating_sub(1))
        }
    }

    pub fn abort(&mut self, why: &str) {
        if !self.aborted {
            self.aborted = true;
            self.stats.aborts += 1;
            if std::env::var_os("TTENGINE_HINT_DEBUG").is_some() {
                eprintln!(
                    "[hint] abort: {} at pc={} op=0x{:02X} depth={} loop={} glyph_attached={}",
                    why,
                    self.last_pc,
                    self.last_op,
                    self.stack.len(),
                    self.gs.loop_count,
                    self.glyph.is_some()
                );
            }
        }
    }

    #[inline]
    pub fn unknown(&mut self, op: u8) {
        self.stats.unknown_opcodes += 1;
        self.stats.last_unknown = Some(op);
    }

    // ---- 点坐标访问(带 reference point / projection 语义的简化版) ----

    /// 取点 p 在"投影向量"上的坐标。本实现投影固定为 x 轴或 y 轴,
    /// 由 `is_x` 选择(由 SFVTCA/SRPVCA 等指令维护在调用参数里)。
    /// prep 阶段(无附着字形)安全返回 0。
    #[inline]
    pub fn point_coord(&self, _p: usize, _is_x: bool) -> F26Dot6 {
        let Some(g) = self.glyph.as_ref() else { return F26Dot6::ZERO };
        if _is_x { g.x(_p) } else { g.y(_p) }
    }

    #[inline]
    pub fn set_point_coord(&mut self, p: usize, is_x: bool, v: F26Dot6) {
        if let Some(g) = self.glyph.as_mut() {
            if is_x { g.set_x(p, v); } else { g.set_y(p, v); }
        }
    }

    #[inline]
    pub fn touch(&mut self, p: usize, is_x: bool) {
        if let Some(g) = self.glyph.as_mut() {
            if is_x { g.touch_x(p); } else { g.touch_y(p); }
        }
    }

    /// IUP(Interpolate Untouched Points):对每条轮廓,
    /// 未触碰点在两个相邻触碰点之间按原始距离比例线性插值;
    /// 轮廓内没有触碰点则整段保持原位。
    pub fn iup_all(&mut self) {
        // 先把 glyph 借用拆出来做(避免借用冲突)
        let Some(g) = self.glyph.as_mut() else { return };
        iup_axis(g, true);
        iup_axis(g, false);
        // IUP 后清除触碰标记
        for t in g.touched.iter_mut() {
            *t = 0;
        }
    }
}

/// 单轴 IUP。`is_x=true` 处理 x 坐标,否则 y 坐标。
fn iup_axis(g: &mut HintedGlyph, is_x: bool) {
    let n = g.num_points();
    if n == 0 {
        return;
    }
    let coord_off = if is_x { 0 } else { n };
    // 先把触碰标记拷出来,避免读写 g.points 时的借用冲突
    let touched: Vec<bool> = (0..n)
        .map(|i| if is_x { g.is_touched_x(i) } else { g.is_touched_y(i) })
        .collect();

    let mut contour_start = 0usize;
    for &end in g.end_pts.iter() {
        let end = end as usize;
        if end < contour_start || end >= n {
            break;
        }
        let len = end - contour_start + 1;
        let touched_idx: Vec<usize> = (contour_start..=end).filter(|&i| touched[i]).collect();
        if touched_idx.is_empty() || touched_idx.len() == len {
            contour_start = end + 1;
            continue;
        }
        // 环上逐段插值:对每个未触碰点,向前/向后找最近触碰点
        for i in contour_start..=end {
            if touched[i] {
                continue;
            }
            let mut next = i;
            let mut next_steps = 0usize;
            loop {
                next = if next == end { contour_start } else { next + 1 };
                next_steps += 1;
                if touched[next] || next == i || next_steps > len {
                    break;
                }
            }
            let mut prev = i;
            let mut prev_steps = 0usize;
            loop {
                prev = if prev == contour_start { end } else { prev - 1 };
                prev_steps += 1;
                if touched[prev] || prev == i || prev_steps > len {
                    break;
                }
            }
            let (p0, p1) = if touched[prev] && touched[next] && prev != next {
                (prev, next)
            } else if touched[next] {
                (next, next)
            } else if touched[prev] {
                (prev, prev)
            } else {
                continue;
            };
            let v0 = g.points[coord_off + p0].raw() as f32;
            let v1 = g.points[coord_off + p1].raw() as f32;
            let total = (prev_steps + next_steps) as f32;
            let t = prev_steps as f32 / total;
            let v = v0 + (v1 - v0) * t;
            g.points[coord_off + i] = F26Dot6(v.round() as i32);
        }
        contour_start = end + 1;
    }
}
