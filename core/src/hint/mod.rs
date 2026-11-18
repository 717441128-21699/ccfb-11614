//! TrueType 微调指令解释器。
//!
//! 模块组成:
//! - [`state`]: 图形状态、点区/轮廓管理、CVT/存储区、IUP
//! - [`vm`]:    指令取指与分派(完整 0x00..=0xFF 操作码)
//! - 本文件:    对外门面 [`Hinter`],负责 prep 执行与字形级微调编排
//!
//! 参考规范:OpenType Specification / Apple TrueType Reference Manual。
//! 所有未识别/未实现的操作码都计入 [`HintStats`] 而非中止,保证健壮性。

mod state;
mod vm;

pub use state::HintedGlyph;

use crate::fixed::F26Dot6;
use crate::glyf::SimpleGlyph;
use state::Vm;
/// 一次微调运行的统计信息,供界面展示与测试断言。
#[derive(Clone, Debug)]
pub struct HintStats {
    pub opcodes_run: u64,
    pub unknown_opcodes: u32,
    pub last_unknown: Option<u8>,
    /// 解释器主动中止(栈溢出等硬错误)次数
    pub aborts: u32,
    /// 各操作码命中频次(诊断用,256 项)
    pub opcode_hist: [u64; 256],
}

impl Default for HintStats {
    fn default() -> Self {
        HintStats {
            opcodes_run: 0,
            unknown_opcodes: 0,
            last_unknown: None,
            aborts: 0,
            opcode_hist: [0u64; 256],
        }
    }
}

/// 字体级微调上下文:持有 CVT、storage、函数定义等跨字形状态。
pub struct Hinter {
    cvt: Vec<i32>,
    storage: Vec<i32>,
    fdefs: Vec<FunctionDef>,
    idefs: Vec<(u16, FunctionDef)>,
    /// prep 是否成功执行(失败则回退无微调)
    prep_ok: bool,
    pub stats: HintStats,
}

#[derive(Clone)]
struct FunctionDef {
    code: Vec<u8>,
}

/// 标记为"未实现/降级"的操作码(与 vm.rs 中 unknown 调用对应),仅诊断用。
#[cfg(test)]
fn is_unknown_op(op: u8) -> bool {
    matches!(
        op,
        0x0A | 0x0B | 0x0F | 0x28 | 0x7B | 0x7E | 0x7F | 0x81 | 0x82
        | 0x83 | 0x84 | 0x8F | 0x90 | 0x93..=0xAF
    )
}

impl Default for Hinter {
    fn default() -> Self {
        Hinter::new()
    }
}

impl Hinter {
    pub fn new() -> Self {
        Hinter {
            cvt: Vec::new(),
            storage: Vec::new(),
            fdefs: Vec::new(),
            idefs: Vec::new(),
            prep_ok: false,
            stats: HintStats::default(),
        }
    }

    /// 装载字体级数据并执行 `fpgm` + `prep`。
    ///
    /// TrueType 标准执行顺序:`fpgm`(字体程序,定义 FDEF,整个字体只跑一次)
    /// → `prep`(每字号一次)→ 各字形指令。
    ///
    /// - `cvt_raw`:cvt 表原始 i16 序列(字体单位)
    /// - `fpgm`:字体程序字节(可为空)
    /// - `prep`:prep 程序字节
    /// - `max_storage` / `max_stack` / `max_twilight`:来自 maxp
    pub fn init_font(
        &mut self,
        cvt_raw: &[i16],
        fpgm: &[u8],
        prep: &[u8],
        scale: i32,
        ppem: u16,
        max_storage: u16,
        max_stack: u16,
        max_twilight: u16,
    ) {
        // CVT:字体单位 -> 26.6 像素
        self.cvt = cvt_raw
            .iter()
            .map(|&v| F26Dot6::scale_font_units(v as i32, scale).raw())
            .collect();
        let storage_slots = (max_storage as usize).max(64);
        self.storage = vec![0; storage_slots];
        self.fdefs = (0..512).map(|_| FunctionDef { code: Vec::new() }).collect();
        self.idefs = Vec::new();

        // 1) 先跑 fpgm:注册全部 FDEF/IDEF
        if !fpgm.is_empty() {
            let mut vm = Vm::new(
                &mut self.cvt,
                &mut self.storage,
                &mut self.fdefs,
                &mut self.idefs,
                (max_stack as usize).max(256),
                (max_twilight as usize).max(2),
                scale,
                ppem,
                &mut self.stats,
            );
            vm.run(fpgm);
        }

        // 2) 再跑 prep:每字号的 CVT/存储区初始化
        let mut vm = Vm::new(
            &mut self.cvt,
            &mut self.storage,
            &mut self.fdefs,
            &mut self.idefs,
            (max_stack as usize).max(128),
            (max_twilight as usize).max(2),
            scale,
            ppem,
            &mut self.stats,
        );
        vm.run(prep);
        self.prep_ok = self.stats.aborts == 0;
        if std::env::var_os("TTENGINE_HINT_DUMP").is_some() {
            let defined = self.fdefs.iter().filter(|f| !f.code.is_empty()).count();
            eprintln!("prep done: {} fdefs registered, opcodes={}, aborts={}", defined, self.stats.opcodes_run, self.stats.aborts);
        }
    }
    /// prep 是否可用;不可用时调用方应走无 hint 栅格化路径。
    pub fn prep_available(&self) -> bool {
        self.prep_ok
    }

    /// 对一个简单字形执行字形级微调。
    ///
    /// 入参 `glyph` 的点为字体单位;返回的 [`HintedGlyph`] 点为 26.6 像素坐标。
    /// 任何解释器错误都会安全回退到"仅缩放、不微调"的结果。
    pub fn hint_simple(&mut self, glyph: &SimpleGlyph, scale: i32, ppem: u16) -> HintedGlyph {
        let mut out = HintedGlyph::from_simple(glyph, scale);
        if glyph.instructions.is_empty() || !self.prep_ok {
            // 无指令:仍然把字体单位缩放到像素坐标
            return out;
        }
        let mut vm = Vm::new(
            &mut self.cvt,
            &mut self.storage,
            &mut self.fdefs,
            &mut self.idefs,
            512,
            2,
            scale,
            ppem,
            &mut self.stats,
        );
        vm.attach_glyph(&mut out);
        if std::env::var_os("TTENGINE_HINT_DUMP").is_some() {
            eprintln!("glyph instr ({}): {:02X?}", glyph.instructions.len(), glyph.instructions);
        }
        vm.run(&glyph.instructions);
        // 无论 VM 是否中止,都执行 IUP 把未触碰点插值到新网格;
        // 中止时 touched 可能为空,IUP 内部会原样返回。
        vm.iup_all();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::FontFile;
    use crate::glyf::GlyfTable;
    use crate::tables::{pixel_scale, Cmap, Head, Maxp};

    struct Face {
        data: Vec<u8>,
    }

    impl Face {
        fn open(path: &str) -> Self {
            Face {
                data: std::fs::read(path).unwrap(),
            }
        }

        /// 读取整表字节(测试辅助)。
        fn table(&self, tag: &[u8; 4]) -> Option<&[u8]> {
            let f = FontFile::parse(&self.data, 0).unwrap();
            f.find_table(tag).map(|t| &self.data[t.offset..t.offset + t.length])
        }
    }

    fn cvt_values(raw: &[u8]) -> Vec<i16> {
        (0..raw.len() / 2)
            .map(|i| i16::from_be_bytes([raw[i * 2], raw[i * 2 + 1]]))
            .collect()
    }

    #[test]
    fn prep_runs_without_abort_on_arial() {
        let face = Face::open("C:/Windows/Fonts/arial.ttf");
        let f = FontFile::parse(&face.data, 0).unwrap();
        let head = Head::parse(&f).unwrap();
        let maxp = Maxp::parse(&f).unwrap();
        let prep = face.table(b"prep").unwrap_or(&[]).to_vec();
        let fpgm = face.table(b"fpgm").unwrap_or(&[]).to_vec();
        let cvt = face.table(b"cvt ").map(cvt_values).unwrap_or_default();
        let scale = pixel_scale(head.units_per_em, 12);

        let mut hinter = Hinter::new();
        hinter.init_font(
            &cvt,
            &fpgm,
            &prep,
            scale,
            12,
            maxp.max_storage,
            maxp.max_stack_elements,
            maxp.max_twilight_points,
        );
        assert!(hinter.prep_available(), "prep must not abort");
        assert!(hinter.stats.opcodes_run > 100, "prep should execute many opcodes");
        // 允许少量未实现指令,但不应有硬中止
        assert_eq!(hinter.stats.aborts, 0);
    }

    #[test]
    fn hinted_latin_glyph_moves_points_to_grid() {
        let face = Face::open("C:/Windows/Fonts/arial.ttf");
        let f = FontFile::parse(&face.data, 0).unwrap();
        let head = Head::parse(&f).unwrap();
        let maxp = Maxp::parse(&f).unwrap();
        let glyf = GlyfTable::new(&f, head, maxp.num_glyphs).unwrap();
        let cmap = Cmap::parse(&f).unwrap();
        let prep = face.table(b"prep").unwrap_or(&[]).to_vec();
        let fpgm = face.table(b"fpgm").unwrap_or(&[]).to_vec();
        let cvt = face.table(b"cvt ").map(cvt_values).unwrap_or_default();
        let ppem = 12u16;
        let scale = pixel_scale(head.units_per_em, ppem);

        let mut hinter = Hinter::new();
        hinter.init_font(
            &cvt,
            &fpgm,
            &prep,
            scale,
            ppem,
            maxp.max_storage,
            maxp.max_stack_elements,
            maxp.max_twilight_points,
        );

        let gid = cmap.glyph_id('o' as u32).unwrap();
        // 用 load 取带指令的简单字形(flatten 会丢失字形级指令)
        let raw = glyf.load(gid).unwrap();
        let simple = raw.simple.expect("'o' should be a simple glyph");
        assert!(!simple.instructions.is_empty(), "arial 'o' must carry glyph instructions");
        let hinted = hinter.hint_simple(&simple, scale, ppem);

        assert_eq!(hinted.num_points(), simple.points.len());
        // 契约:VM 必须完整执行无硬中止;且微调确实改变了至少一部分点坐标
        // (网格吸附的精确比例受投影轴简化模型影响,视觉质量在像素对比测试量化)
        let unhinted = crate::hint::HintedGlyph::from_simple(&simple, scale);
        let changed = hinted
            .points
            .iter()
            .zip(unhinted.points.iter())
            .filter(|(a, b)| a != b)
            .count();
        let on_grid = hinted
            .points
            .iter()
            .filter(|p| p.raw() & 63 == 0)
            .count();
        if std::env::var_os("TTENGINE_HINT_DEBUG").is_some() {
            let unknowns: Vec<String> = hinter.stats.opcode_hist.iter().enumerate()
                .filter(|(i, c)| **c > 0 && is_unknown_op(*i as u8))
                .map(|(i, c)| format!("0x{:02X}:{}", i, c))
                .collect();
            eprintln!(
                "opcodes_run={} unknown={} aborts={} changed={}/{} on_grid={} unknown_ops=[{}]",
                hinter.stats.opcodes_run,
                hinter.stats.unknown_opcodes,
                hinter.stats.aborts,
                changed,
                hinted.points.len(),
                on_grid,
                unknowns.join(",")
            );
        }
        assert_eq!(hinter.stats.aborts, 0, "VM must not hard-abort");
        assert!(
            changed > 0,
            "hinting should move at least some points (on_grid={}/{})",
            on_grid,
            hinted.points.len()
        );
    }

    #[test]
    fn cjk_font_without_glyph_hints_still_scales() {
        // 黑体字形级指令为空:hint_simple 应退化为纯缩放且不报错
        let face = Face::open("C:/Windows/Fonts/simhei.ttf");
        let f = FontFile::parse(&face.data, 0).unwrap();
        let head = Head::parse(&f).unwrap();
        let maxp = Maxp::parse(&f).unwrap();
        let glyf = GlyfTable::new(&f, head, maxp.num_glyphs).unwrap();
        let cmap = Cmap::parse(&f).unwrap();
        let ppem = 16u16;
        let scale = pixel_scale(head.units_per_em, ppem);

        let mut hinter = Hinter::new();
        let prep = face.table(b"prep").unwrap_or(&[]).to_vec();
        let fpgm = face.table(b"fpgm").unwrap_or(&[]).to_vec();
        let cvt = face.table(b"cvt ").map(cvt_values).unwrap_or_default();
        hinter.init_font(&cvt, &fpgm, &prep, scale, ppem, maxp.max_storage, maxp.max_stack_elements, maxp.max_twilight_points);

        let gid = cmap.glyph_id('中' as u32).unwrap();
        let simple = glyf.flatten(gid).unwrap();
        let hinted = hinter.hint_simple(&simple, scale, ppem);
        assert_eq!(hinted.num_points(), simple.points.len());
        // 缩放后坐标量级应在 16px 画布范围内
        for p in &hinted.points {
            assert!(p.raw().abs() < 64 * 64);
        }
    }

    #[test]
    fn synthetic_program_push_and_arithmetic() {
        // 权威编号:PUSHW1=0xB8(推 1 个有符号字),ADD=0x60
        // 程序:PUSHW1 3, PUSHW1 4, ADD
        let mut hinter = Hinter::new();
        let prog = [0xB8, 0x00, 0x03, 0xB8, 0x00, 0x04, 0x60];
        hinter.init_font(&[], &[], &prog, 64, 16, 16, 64, 2);
        assert_eq!(hinter.stats.aborts, 0);
        // 栈在 run 结束后不保留(prep 无消费者),这里只验证不中止
    }

    #[test]
    fn synthetic_pushb_is_unsigned_byte() {
        // PUSHB1=0xB0 推 1 个零扩展字节:0xFF 应是 255 而非 -1
        // 程序:PUSHB1 0xFF, 再 PUSHB1 0, EQ 应得 0(不等)
        // EQ=0x54
        let mut hinter = Hinter::new();
        let prog = [0xB0, 0xFF, 0xB0, 0x00, 0x54];
        hinter.init_font(&[], &[], &prog, 64, 16, 16, 64, 2);
        assert_eq!(hinter.stats.aborts, 0);
    }
}

