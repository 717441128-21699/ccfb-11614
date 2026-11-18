//! TrueType 指令分派(完整 0x00..=0xFF)。
//!
//! 操作码编号严格对齐 read-fonts / FreeType 的权威 256 项映射,
//! 与 OpenType 规范一致。投影/自由向量退化为坐标轴方向,
//! 斜向向量指令(SFVFS/SPVFS/SANGW 等)安全降级并计入统计。
//! 任何未实现或畸形情况只记统计、绝不 panic。

use super::state::{CallFrame, RoundState, Vm};
use crate::fixed::F26Dot6;

impl<'a> Vm<'a> {
    /// 执行一段程序(prep 或字形指令)。中止后立即返回。
    pub fn run(&mut self, code: &[u8]) {
        let mut pc = 0usize;
        let trace = std::env::var_os("TTENGINE_HINT_TRACE").is_some();
        while pc < code.len() && !self.aborted {
            let op = code[pc];
            self.last_pc = pc;
            self.last_op = op;
            if trace && (pc < 400 || op == 0x2C || op == 0x2D || op == 0x1C) {
                eprintln!("pc={:3} op=0x{:02X} depth={} loop={}", pc, op, self.stack.len(), self.gs.loop_count);
            }
            pc += 1;
            self.stats.opcodes_run += 1;
            self.stats.opcode_hist[op as usize] += 1;
            pc = self.dispatch(op, code, pc);
        }
    }

    fn dispatch(&mut self, op: u8, code: &[u8], pc: usize) -> usize {
        match op {
            // ============ 0x00-0x0F:向量/几何 ============
            // SVTCA:投影与自由向量同轴
            0x00 => self.set_both_axes(false, pc), // y
            0x01 => self.set_both_axes(true, pc),  // x
            // SPVTCA:只设投影轴
            0x02 => { self.projection_is_x = false; pc }
            0x03 => { self.projection_is_x = true; pc }
            // SFVTCA:只设自由轴
            0x04 => { self.freedom_is_x = false; pc }
            0x05 => { self.freedom_is_x = true; pc }
            // SPVTL:投影轴(正交,坐标轴退化等价)
            0x06 => { self.projection_is_x = false; pc }
            0x07 => { self.projection_is_x = true; pc }
            // SFVTL:自由轴
            0x08 => { self.freedom_is_x = false; pc }
            0x09 => { self.freedom_is_x = true; pc }
            // SPVFS/SFVFS:斜向向量,弹 2 栈后降级
            0x0A | 0x0B => { self.pop(); self.pop(); self.unknown(op); pc }
            // GPV/GFV:把当前投影/自由向量压栈(坐标轴退化:轴单位向量)
            0x0C => { self.push(if self.projection_is_x { 0x4000 } else { 0 }); self.push(if self.projection_is_x { 0 } else { 0x4000 }); pc }
            0x0D => { self.push(if self.freedom_is_x { 0x4000 } else { 0 }); self.push(if self.freedom_is_x { 0 } else { 0x4000 }); pc }
            // SFVTPV:自由向量=投影向量
            0x0E => { self.freedom_is_x = self.projection_is_x; pc }
            // ISECT:两线段交点(罕见,弹 4 点后降级)
            0x0F => { for _ in 0..4 { self.pop(); } self.unknown(op); pc }

            // ============ 0x10-0x17:参考点/zone/loop ============
            0x10 => { self.gs.rp[0] = self.pop_point(); pc } // SRP0
            0x11 => { self.gs.rp[1] = self.pop_point(); pc } // SRP1
            0x12 => { self.gs.rp[1] = self.pop_point(); self.gs.rp[0] = self.gs.rp[1]; pc } // SRP2(简化:rp0=rp1=点)
            0x13 => { self.gs.gep[0] = F26Dot6(self.pop()); pc } // SZP0
            0x14 => { self.gs.gep[1] = F26Dot6(self.pop()); pc } // SZP1
            0x15 => { let v = F26Dot6(self.pop()); self.gs.gep[1] = v; self.gs.gep[0] = v; pc } // SZP2
            0x16 => { let v = F26Dot6(self.pop()); self.gs.gep = [v, v]; pc } // SZPS
            0x17 => { self.gs.loop_count = self.pop(); pc } // SLOOP

            // ============ 0x18-0x1F:舍入/距离/控制流 ============
            0x18 => self.grid_round(pc, F26Dot6::HALF),  // RTG
            0x19 => self.grid_round(pc, F26Dot6::HALF),  // RTHG
            0x1A => { self.gs.minimum_distance = F26Dot6(self.pop()); pc } // SMD
            0x1B => pc, // ELSE:顺序流里直接跨过
            0x1C => { let off = self.pop(); jump_rel(pc, off) } // JMPR
            0x1D => { self.gs.control_value_cut_in = F26Dot6(self.pop()); pc } // SCVTCI
            0x1E => { self.gs.single_width_cut_in = F26Dot6(self.pop()); pc } // SSWCI
            0x1F => { self.gs.single_width_value = F26Dot6(self.pop()); pc } // SSW

            // ============ 0x20-0x27:栈管理 ============
            0x20 => { let v = self.pop(); self.push(v); self.push(v); pc } // DUP
            0x21 => { self.pop(); pc } // POP
            0x22 => { self.stack.clear(); pc } // CLEAR
            0x23 => { let n = self.stack.len(); if n >= 2 { self.stack.swap(n - 1, n - 2); } pc } // SWAP
            0x24 => { self.push(self.stack.len() as i32); pc } // DEPTH
            0x25 => { // CINDEX
                let k = self.pop().max(1) as usize;
                if k <= self.stack.len() { let v = self.stack[self.stack.len() - k]; self.push(v); }
                pc
            }
            0x26 => { // MINDEX
                let k = self.pop().max(1) as usize;
                if k <= self.stack.len() { let i = self.stack.len() - k; let v = self.stack.remove(i); self.push(v); }
                pc
            }
            0x27 => self.op_alignpts(pc), // ALIGNPTS

            // ============ 0x28-0x2D:函数/控制流 ============
            0x28 => { self.unknown(op); pc }
            0x29 => self.op_utp(pc),    // UTP
            0x2A => self.loop_call(pc), // LOOPCALL
            0x2B => self.call_function(pc), // CALL
            0x2C => self.def_function(pc, code), // FDEF
            0x2D => code.len(), // ENDF

            // ============ 0x2E-0x2F:MDAP ============
            0x2E => self.op_mdap(pc, false), // MDAP0(无舍入)
            0x2F => self.op_mdap(pc, true),  // MDAP1(舍入)

            // ============ 0x30-0x31:IUP(统一在程序结束后做,此处 no-op) ============
            0x30 | 0x31 => pc,

            // ============ 0x32-0x37:SHP/SHC/SHZ ============
            0x32 | 0x33 => self.op_shp(pc),
            0x34 | 0x35 => self.op_shc(pc),
            0x36 | 0x37 => self.op_shz(pc),

            // ============ 0x38-0x3C:点移动 ============
            0x38 => self.op_shpix(pc), // SHPIX
            0x39 => self.op_ip(pc),    // IP
            0x3A | 0x3B => self.op_msirp(pc), // MSIRP
            0x3C => self.op_alignrp(pc), // ALIGNRP

            // ============ 0x3D-0x3F:RTDG/MIAP ============
            0x3D => self.grid_round(pc, F26Dot6::ZERO), // RTDG
            0x3E => self.op_miap(pc, false), // MIAP0
            0x3F => self.op_miap(pc, true),  // MIAP1

            // ============ 0x40-0x45:Push / 存储 / CVT ============
            0x40 => { // NPUSHB
                let n = code.get(pc).copied().unwrap_or(0) as usize;
                self.push_n_bytes(n, code, pc + 1)
            }
            0x41 => { // NPUSHW
                let n = code.get(pc).copied().unwrap_or(0) as usize;
                self.push_n_words(n, code, pc + 1)
            }
            0x42 => { // WS
                let idx = self.pop() as usize; let val = self.pop();
                if idx < self.storage.len() { self.storage[idx] = val; }
                pc
            }
            0x43 => { // RS
                let idx = self.pop() as usize;
                self.push(if idx < self.storage.len() { self.storage[idx] } else { 0 });
                pc
            }
            0x44 => self.wcvt(pc, false), // WCVTP
            0x45 => { // RCVT
                let idx = self.pop() as usize;
                self.push(if idx < self.cvt.len() { self.cvt[idx] } else { 0 });
                pc
            }

            // ============ 0x46-0x4F:环境 ============
            0x46 | 0x47 => { // GC0/GC1:取投影/自由分量(坐标轴退化)
                let p = self.pop_point();
                let v = if (op == 0x46) == self.projection_is_x { self.point_coord(p, true) } else { self.point_coord(p, false) };
                self.push(v.raw());
                pc
            }
            0x48 => { // SCFS:单宽度
                let p = self.pop_point();
                let dist = F26Dot6(self.pop());
                let is_x = self.freedom_is_x;
                let cur = self.point_coord(p, is_x);
                self.set_point_coord(p, is_x, F26Dot6(cur.raw() + dist.raw()));
                self.touch(p, is_x);
                pc
            }
            0x49 | 0x4A => { // MD0/MD1:测量距离(坐标轴退化)
                let p1 = self.pop_point(); let p0 = self.pop_point();
                let is_x = if op == 0x49 { self.freedom_is_x } else { self.projection_is_x };
                let d = self.point_coord(p1, is_x).raw() - self.point_coord(p0, is_x).raw();
                self.push(d);
                pc
            }
            0x4B => { self.push(self.ppem as i32); pc } // MPPEM
            0x4C => { self.push(self.ppem as i32); pc } // MPS(点尺寸,简化同 ppem)
            0x4D => { if let Some(g) = self.glyph.as_mut() { for v in g.on_curve.iter_mut() { *v = true; } } pc } // FLIPON
            0x4E => { if let Some(g) = self.glyph.as_mut() { for v in g.on_curve.iter_mut() { *v = false; } } pc } // FLIPOFF
            0x4F => { self.pop(); pc } // DEBUG

            // ============ 0x50-0x57:比较 ============
            0x50 => self.binop(pc, |a, b| if a < b { 1 } else { 0 }),  // LT
            0x51 => self.binop(pc, |a, b| if a <= b { 1 } else { 0 }), // LTEQ
            0x52 => self.binop(pc, |a, b| if a > b { 1 } else { 0 }),  // GT
            0x53 => self.binop(pc, |a, b| if a >= b { 1 } else { 0 }), // GTEQ
            0x54 => self.binop(pc, |a, b| if a == b { 1 } else { 0 }), // EQ
            0x55 => self.binop(pc, |a, b| if a != b { 1 } else { 0 }), // NEQ
            0x56 => { let v = self.pop(); self.push((v & !1) + 1); pc } // ODD
            0x57 => { let v = self.pop(); self.push(v & !1); pc }       // EVEN

            // ============ 0x58-0x5C:条件/逻辑 ============
            0x58 => { // IF
                let cond = self.pop();
                if cond != 0 { pc } else { self.skip_if_block(code, pc) }
            }
            0x59 => pc, // EIF
            0x5A => self.binop(pc, |a, b| a & b), // AND
            0x5B => self.binop(pc, |a, b| a | b), // OR
            0x5C => { let v = self.pop(); self.push(if v == 0 { 1 } else { 0 }); pc } // NOT

            // ============ 0x5D-0x5F:Delta / delta 配置 ============
            0x5D => self.op_deltap(code, pc, 0), // DELTAP1
            0x5E => { self.gs.delta_base = self.pop(); pc } // SDB
            0x5F => { self.gs.delta_shift = self.pop(); pc } // SDS

            // ============ 0x60-0x67:算术 ============
            0x60 => self.binop(pc, |a, b| a.wrapping_add(b)), // ADD
            0x61 => self.binop(pc, |a, b| a.wrapping_sub(b)), // SUB
            0x62 => { // DIV
                let b = self.pop() as i64; let a = self.pop() as i64;
                self.push(if b == 0 { 0 } else { ((a << 6) / b) as i32 }); pc
            }
            0x63 => { // MUL(26.6)
                let b = self.pop() as i64; let a = self.pop() as i64;
                self.push(((a * b) >> 6) as i32); pc
            }
            0x64 => { let v = self.pop(); self.push(v.abs()); pc } // ABS
            0x65 => { let v = self.pop(); self.push(-v); pc }       // NEG
            0x66 => { let v = F26Dot6(self.pop()); self.push(v.floor().raw()); pc } // FLOOR
            0x67 => { let v = F26Dot6(self.pop()); self.push(v.round().raw()); pc } // CEILING(近似 round)

            // ============ 0x68-0x6F:ROUND/NROUND 四变体 ============
            0x68..=0x6B => self.op_round_variant(op - 0x68, pc, false), // ROUND
            0x6C..=0x6F => self.op_round_variant(op - 0x6C, pc, true),  // NROUND

            // ============ 0x70-0x77 ============
            0x70 => self.wcvt(pc, true), // WCVTF
            0x71 => self.op_deltap(code, pc, 1), // DELTAP2
            0x72 => self.op_deltap(code, pc, 2), // DELTAP3
            0x73 | 0x74 | 0x75 => self.op_deltac(code, pc, op - 0x73), // DELTAC1/2/3
            0x76 => { self.gs.round_state = decode_round(self.pop()); pc } // SROUND
            0x77 => { self.gs.round_state = decode_round(self.pop()); pc } // S45ROUND

            // ============ 0x78-0x7F:条件跳转/角度/AA ============
            0x78 => { let off = self.pop(); let c = self.pop(); if c != 0 { jump_rel(pc, off) } else { pc } } // JROT
            0x79 => { let off = self.pop(); let c = self.pop(); if c == 0 { jump_rel(pc, off) } else { pc } } // JROF
            0x7A => { let off = self.pop(); jump_rel(pc, off) } // ROFF
            0x7B => { self.unknown(op); pc }
            0x7C => self.grid_round(pc, F26Dot6::HALF), // RUTG
            0x7D => self.grid_round(pc, F26Dot6::ZERO), // RDTG
            0x7E => { self.pop(); self.pop(); self.unknown(op); pc } // SANGW
            0x7F => { self.pop(); self.unknown(op); pc } // AA

            // ============ 0x80-0x8F:点翻转/扫描控制/IDEF/MAX/MIN ============
            0x80 => { // FLIPPT
                let n = self.pop().max(0) as usize;
                let idxs: Vec<usize> = (0..n).map(|_| self.pop() as usize).collect();
                if let Some(g) = self.glyph.as_mut() {
                    let count = g.num_points();
                    for p in idxs { if p < count { g.on_curve[p] = !g.on_curve[p]; } }
                }
                pc
            }
            0x81 | 0x82 => { self.pop(); self.unknown(op); pc } // FLIPRGON/OFF
            0x83 | 0x84 => { self.unknown(op); pc }
            0x85 => { self.pop(); pc } // SCANCTRL
            0x86 => { self.projection_is_x = false; pc } // SDPVTL0
            0x87 => { self.projection_is_x = true; pc }  // SDPVTL1
            0x88 => { let _sel = self.pop(); self.push(0x22 | (1 << 5) | (1 << 8)); pc } // GETINFO
            0x89 => self.def_instruction(pc, code), // IDEF
            0x8A => { // ROLL
                if self.stack.len() >= 3 { let top = self.stack.len() - 1; let v = self.stack.remove(top - 2); self.stack.push(v); }
                pc
            }
            0x8B => self.binop(pc, i32::max), // MAX
            0x8C => self.binop(pc, i32::min), // MIN
            0x8D => { self.pop(); pc } // SCANTYPE
            0x8E => { self.pop(); self.pop(); pc } // INSTCTRL
            0x8F => { self.unknown(op); pc }

            // ============ 0x90-0x9F / 0xA0-0xAF:扩展/保留,安全跳过 ============
            0x90 => { self.unknown(op); pc }
            0x91 => { self.pop(); self.push(0); pc } // GETVARIATION(可变字体未支持)
            0x92 => { self.pop(); self.push(0); pc } // GETDATA
            0x93..=0xAF => { self.unknown(op); pc }

            // ============ 0xB0-0xBF:定长 Push ============
            // B0-B7:推 1..8 个字节;B8-BF:推 1..8 个字
            0xB0..=0xB7 => self.push_n_bytes(1 + (op - 0xB0) as usize, code, pc),
            0xB8..=0xBF => self.push_n_words(1 + (op - 0xB8) as usize, code, pc),

            // ============ 0xC0-0xDF:MDRP(直接相对移动) ============
            0xC0..=0xDF => self.op_mdrp(op, pc),
            // ============ 0xE0-0xFF:MIRP(间接相对移动,带 CVT) ============
            0xE0..=0xFF => self.op_mirp(op, pc),
        }
    }

    fn set_both_axes(&mut self, is_x: bool, pc: usize) -> usize {
        self.projection_is_x = is_x;
        self.freedom_is_x = is_x;
        pc
    }

    fn grid_round(&mut self, pc: usize, threshold: F26Dot6) -> usize {
        self.gs.round_state = RoundState { period: F26Dot6::ONE, phase: F26Dot6::ZERO, threshold };
        pc
    }

    fn binop<F: Fn(i32, i32) -> i32>(&mut self, pc: usize, f: F) -> usize {
        let b = self.pop();
        let a = self.pop();
        self.push(f(a, b));
        pc
    }

    // ---------- Push ----------

    fn push_n_bytes(&mut self, n: usize, code: &[u8], pc: usize) -> usize {
        let mut p = pc;
        for _ in 0..n {
            if p >= code.len() { self.abort("truncated pushb"); return code.len(); }
            // PUSHB:零扩展(无符号字节)
            self.push(code[p] as i32);
            p += 1;
        }
        p
    }

    fn push_n_words(&mut self, n: usize, code: &[u8], pc: usize) -> usize {
        let mut p = pc;
        for _ in 0..n {
            if p + 1 >= code.len() { self.abort("truncated pushw"); return code.len(); }
            self.push(i16::from_be_bytes([code[p], code[p + 1]]) as i32);
            p += 2;
        }
        p
    }

    // ---------- 控制流 ----------

    fn skip_if_block(&mut self, code: &[u8], pc: usize) -> usize {
        let mut depth = 1i32;
        let mut p = pc;
        while p < code.len() && depth > 0 {
            match code[p] {
                0x58 => depth += 1,
                0x59 => depth -= 1,
                0x1B => if depth == 1 { return p + 1 },
                _ => {}
            }
            p += 1;
        }
        p
    }

    fn def_function(&mut self, pc: usize, code: &[u8]) -> usize {
        let num = self.pop() as usize;
        let mut end = pc;
        while end < code.len() && code[end] != 0x2D { end += 1; }
        if std::env::var_os("TTENGINE_HINT_DUMP").is_some() {
            eprintln!("FDEF num={} body=[{},{}] end_found={}", num, pc, end, end < code.len());
        }
        if num < self.fdefs.len() {
            self.fdefs[num].code = code[pc..end].to_vec();
        }
        end + 1
    }

    fn def_instruction(&mut self, pc: usize, code: &[u8]) -> usize {
        let num = self.pop();
        let mut end = pc;
        while end < code.len() && code[end] != 0x2D { end += 1; }
        let body = code[pc..end].to_vec();
        if let Some(slot) = self.idefs.iter_mut().find(|(n, _)| *n == num as u16) {
            slot.1.code = body;
        } else if self.idefs.len() < 64 {
            self.idefs.push((num as u16, super::FunctionDef { code: body }));
        }
        end + 1
    }

    fn call_function(&mut self, pc: usize) -> usize {
        let num = self.pop() as usize;
        let defined = num < self.fdefs.len() && !self.fdefs[num].code.is_empty();
        if std::env::var_os("TTENGINE_HINT_TRACE").is_some() && !defined {
            eprintln!("    CALL fdef {} NOT defined (have {} slots)", num, self.fdefs.len());
        }
        if defined {
            let body = self.fdefs[num].code.clone();
            self.call_stack.push(CallFrame { return_addr: pc, saved_state: self.gs });
            self.run(&body);
            if let Some(frame) = self.call_stack.pop() {
                self.gs = frame.saved_state;
                return frame.return_addr;
            }
        }
        pc
    }

    fn loop_call(&mut self, pc: usize) -> usize {
        let count = self.gs.loop_count.max(1);
        self.gs.loop_count = 1;
        let num = self.pop() as usize;
        if num < self.fdefs.len() && !self.fdefs[num].code.is_empty() {
            let body = self.fdefs[num].code.clone();
            for _ in 0..count {
                if self.aborted { break; }
                self.call_stack.push(CallFrame { return_addr: pc, saved_state: self.gs });
                self.run(&body);
                if let Some(frame) = self.call_stack.pop() { self.gs = frame.saved_state; }
            }
        }
        pc
    }

    fn wcvt(&mut self, pc: usize, font_units: bool) -> usize {
        let idx = self.pop() as usize;
        let val = self.pop();
        let v = if font_units { F26Dot6::scale_font_units(val, self.scale).raw() } else { val };
        if idx < self.cvt.len() { self.cvt[idx] = v; }
        pc
    }

    fn op_round_variant(&mut self, variant: u8, pc: usize, is_nround: bool) -> usize {
        let d = F26Dot6(self.pop());
        let out = if is_nround {
            self.gs.engine_distance_variant(d, variant | 2)
        } else {
            self.gs.engine_distance_variant(d, variant)
        };
        self.push(out.raw());
        pc
    }

    // ---------- 点移动 ----------

    /// ALIGNPTS:把若干对点对齐(沿投影轴)。
    fn op_alignpts(&mut self, pc: usize) -> usize {
        let n = self.gs.loop_count.max(1) as usize;
        self.gs.loop_count = 1;
        let is_x = self.projection_is_x;
        for _ in 0..n {
            let p1 = self.pop_point();
            let p0 = self.pop_point();
            let target = self.point_coord(p0, is_x);
            self.set_point_coord(p1, is_x, target);
            self.touch(p1, is_x);
        }
        pc
    }

    /// UTP:取消触碰。
    fn op_utp(&mut self, pc: usize) -> usize {
        let n = self.gs.loop_count.max(1) as usize;
        self.gs.loop_count = 1;
        let is_x = self.projection_is_x;
        for _ in 0..n {
            let p = self.pop_point();
            if let Some(g) = self.glyph.as_mut() {
                if is_x { g.untouch_x(p); } else { g.untouch_y(p); }
            }
        }
        pc
    }

    /// MDAP:把点移到其当前坐标(用于标记参考点),round 变体做网格吸附。
    fn op_mdap(&mut self, pc: usize, do_round: bool) -> usize {
        let p = self.pop_point();
        let is_x = self.projection_is_x;
        let cur = self.point_coord(p, is_x);
        if do_round {
            let r = self.gs.round(cur);
            self.set_point_coord(p, is_x, r);
        }
        self.touch(p, is_x);
        self.gs.rp = [p, p];
        pc
    }

    /// MIAP:移动点到绝对坐标(可经 CVT cut-in 修正)。
    fn op_miap(&mut self, pc: usize, do_round: bool) -> usize {
        let p = self.pop_point();
        let mut target = F26Dot6(self.pop());
        // 栈顶可能还有一个 CVT 索引(规范里 MIAP 只弹 point+value,
        // CVT 选择由调用方预先 RCVT 完成,这里不额外消费)
        let is_x = self.projection_is_x;
        let cur = self.point_coord(p, is_x);
        let mut d = target.raw() - cur.raw();
        if do_round {
            d = self.gs.round(F26Dot6(d)).raw();
        }
        self.set_point_coord(p, is_x, F26Dot6(cur.raw() + d));
        self.touch(p, is_x);
        self.gs.rp = [p, p];
        let _ = &mut target;
        pc
    }

    /// SHP:把一组点整体平移,使其参考点对齐到 rp。
    ///
    /// 关键:计算"参考点当前位置 → 目标参考点 rp"的位移 delta,
    /// 再把同一 delta 施加到每个点,保持点之间的相对几何,
    /// 绝不能把所有点都设成同一个绝对坐标(那会让字形坍缩)。
    fn op_shp(&mut self, pc: usize) -> usize {
        let n = self.gs.loop_count.max(1) as usize;
        self.gs.loop_count = 1;
        let is_x = self.projection_is_x;
        let pts: Vec<usize> = (0..n).map(|_| self.pop_point()).collect();
        let ref_pt = self.pop_point();
        let rp = self.gs.rp[0];
        let delta = self.point_coord(rp, is_x).raw() - self.point_coord(ref_pt, is_x).raw();
        for p in pts {
            let cur = self.point_coord(p, is_x).raw();
            self.set_point_coord(p, is_x, F26Dot6(cur + delta));
            self.touch(p, is_x);
        }
        pc
    }

    /// SHC:整条轮廓平移固定位移。
    fn op_shc(&mut self, pc: usize) -> usize {
        let n = self.gs.loop_count.max(1) as usize;
        self.gs.loop_count = 1;
        let is_x = self.freedom_is_x;
        let contours: Vec<usize> = (0..n).map(|_| self.pop_point()).collect();
        let shift = F26Dot6(self.pop());
        if let Some(g) = self.glyph.as_mut() {
            for c in contours {
                let (start, end) = contour_range(g, c);
                for p in start..=end {
                    let cur = if is_x { g.x(p) } else { g.y(p) };
                    if is_x { g.set_x(p, F26Dot6(cur.raw() + shift.raw())); g.touch_x(p); }
                    else { g.set_y(p, F26Dot6(cur.raw() + shift.raw())); g.touch_y(p); }
                }
            }
        }
        pc
    }

    /// SHZ:整条轮廓按"轮廓参考点 → rp"的位移整体平移(保持轮廓形状)。
    fn op_shz(&mut self, pc: usize) -> usize {
        let n = self.gs.loop_count.max(1) as usize;
        self.gs.loop_count = 1;
        let is_x = self.projection_is_x;
        let contours: Vec<usize> = (0..n).map(|_| self.pop_point()).collect();
        let ref_pt = self.pop_point();
        let rp = self.gs.rp[0];
        let delta = self.point_coord(rp, is_x).raw() - self.point_coord(ref_pt, is_x).raw();
        if let Some(g) = self.glyph.as_mut() {
            for c in contours {
                let (start, end) = contour_range(g, c);
                for p in start..=end {
                    let cur = if is_x { g.x(p) } else { g.y(p) };
                    if is_x { g.set_x(p, F26Dot6(cur.raw() + delta)); g.touch_x(p); }
                    else { g.set_y(p, F26Dot6(cur.raw() + delta)); g.touch_y(p); }
                }
            }
        }
        pc
    }

    /// SHPIX:沿自由轴移动若干点固定位移。
    fn op_shpix(&mut self, pc: usize) -> usize {
        let n = self.gs.loop_count.max(1) as usize;
        self.gs.loop_count = 1;
        let is_x = self.freedom_is_x;
        let pts: Vec<usize> = (0..n).map(|_| self.pop_point()).collect();
        let shift = F26Dot6(self.pop());
        for p in pts {
            let cur = self.point_coord(p, is_x);
            self.set_point_coord(p, is_x, F26Dot6(cur.raw() + shift.raw()));
            self.touch(p, is_x);
        }
        pc
    }

    /// IP:两点间插值移动。
    fn op_ip(&mut self, pc: usize) -> usize {
        let n = self.gs.loop_count.max(1) as usize;
        self.gs.loop_count = 1;
        let is_x = self.projection_is_x;
        for _ in 0..n {
            let p1 = self.pop_point();
            let p0 = self.pop_point();
            let dist = F26Dot6(self.pop());
            let base = self.point_coord(p0, is_x).raw();
            self.set_point_coord(p1, is_x, F26Dot6(base + dist.raw()));
            self.touch(p1, is_x);
        }
        pc
    }

    /// MSIRP(0x3A/0x3B):栈序 (p 底, d 顶),先弹 distance 再弹 point。
    fn op_msirp(&mut self, pc: usize) -> usize {
        let distance = F26Dot6(self.pop());
        let p = self.pop_point();
        let is_x = self.projection_is_x;
        let rp0 = self.gs.rp[0];
        let base = self.point_coord(rp0, is_x).raw();
        self.set_point_coord(p, is_x, F26Dot6(base + distance.raw()));
        self.touch(p, is_x);
        self.gs.rp[1] = p;
        pc
    }

    /// ALIGNRP:把点对齐到 rp0。
    fn op_alignrp(&mut self, pc: usize) -> usize {
        let n = self.gs.loop_count.max(1) as usize;
        self.gs.loop_count = 1;
        let is_x = self.projection_is_x;
        for _ in 0..n {
            let p1 = self.pop_point();
            let p0 = self.gs.rp[0];
            let target = self.point_coord(p0, is_x);
            self.set_point_coord(p1, is_x, target);
            self.touch(p1, is_x);
            self.gs.rp[1] = p1;
        }
        pc
    }

    /// MDRP(0xC0-0xDF):Move Direct Relative Point。
    /// 栈上只有点号 p;距离隐式取自原始轮廓中 p 相对 rp0 的距离。
    /// 低 5 位标志:bit0=round, bit1-2=rp 更新, bit4=不修改距离。
    fn op_mdrp(&mut self, op: u8, pc: usize) -> usize {
        let p = self.pop_point();
        let is_x = self.projection_is_x;
        let rp0 = self.gs.rp[0];
        // 隐式距离:点 p 与 rp0 在投影轴上的当前距离
        let mut distance = self.point_coord(p, is_x).raw() - self.point_coord(rp0, is_x).raw();
        if op & 0x01 != 0 {
            distance = self.gs.round(F26Dot6(distance)).raw();
        }
        let base = self.point_coord(rp0, is_x).raw();
        self.set_point_coord(p, is_x, F26Dot6(base + distance));
        self.touch(p, is_x);
        self.apply_rp_flags(op, p);
        pc
    }

    /// MIRP(0xE0-0xFF):间接相对移动,距离先经 CVT cut-in。
    fn op_mirp(&mut self, op: u8, pc: usize) -> usize {
        let cvt_idx = self.pop() as usize;
        let p = self.pop_point();
        let is_x = self.projection_is_x;
        let rp0 = self.gs.rp[0];
        let mut distance = if cvt_idx < self.cvt.len() { self.cvt[cvt_idx] } else { 0 };
        let ref_dist = self.point_coord(p, is_x).raw() - self.point_coord(rp0, is_x).raw();
        if (distance - ref_dist).abs() > self.gs.control_value_cut_in.raw() {
            distance = ref_dist;
        }
        if op & 0x01 != 0 {
            distance = self.gs.round(F26Dot6(distance)).raw();
        }
        let base = self.point_coord(rp0, is_x).raw();
        self.set_point_coord(p, is_x, F26Dot6(base + distance));
        self.touch(p, is_x);
        self.apply_rp_flags(op, p);
        pc
    }

    /// MDRP/MIRP 低 2 位决定参考点更新规则。
    fn apply_rp_flags(&mut self, op: u8, p: usize) {
        match op & 0x03 {
            0 => self.gs.rp[1] = p,
            1 => { self.gs.rp[0] = p; self.gs.rp[1] = p; }
            2 => self.gs.rp[1] = self.gs.rp[0],
            _ => {}
        }
    }

    /// DELTAP1/2/3:点级像素微调。
    fn op_deltap(&mut self, code: &[u8], pc: usize, band: u8) -> usize {
        let count = self.pop().max(0) as usize;
        let base = self.gs.delta_base + band as i32 * 16;
        let shift = self.gs.delta_shift;
        let is_x = self.freedom_is_x;
        let mut p = pc;
        for _ in 0..count {
            if p + 1 >= code.len() { break; }
            let point = code[p] as usize;
            let mut mag = code[p + 1] as i32;
            p += 2;
            // 每 16 项一个 ppem 门控字节
            if mag >= 0x80 { p += 1; mag &= 0x7F; }
            if mag == 0 { continue; }
            let mag = mag - 64;
            let shift_amt = (base - shift).clamp(0, 30) as u32;
            let delta = mag.wrapping_mul(1i32 << shift_amt);
            if let Some(g) = self.glyph.as_mut() {
                if point < g.num_points() {
                    let cur = if is_x { g.x(point) } else { g.y(point) };
                    if is_x { g.set_x(point, F26Dot6(cur.raw().wrapping_add(delta))); }
                    else { g.set_y(point, F26Dot6(cur.raw().wrapping_add(delta))); }
                }
            }
        }
        p
    }

    /// DELTAC1/2/3:CVT 微调。
    fn op_deltac(&mut self, code: &[u8], pc: usize, band: u8) -> usize {
        let count = self.pop().max(0) as usize;
        let base = self.gs.delta_base + band as i32 * 16;
        let shift = self.gs.delta_shift;
        let mut p = pc;
        for _ in 0..count {
            if p + 1 >= code.len() { break; }
            let idx = code[p] as usize;
            let mut mag = code[p + 1] as i32;
            p += 2;
            if mag >= 0x80 { p += 1; mag &= 0x7F; }
            if mag == 0 { continue; }
            let mag = mag - 64;
            let shift_amt = (base - shift).clamp(0, 30) as u32;
            let delta = mag.wrapping_mul(1i32 << shift_amt);
            if idx < self.cvt.len() { self.cvt[idx] = self.cvt[idx].wrapping_add(delta); }
        }
        p
    }
}

fn contour_range(g: &super::HintedGlyph, contour_idx: usize) -> (usize, usize) {
    if contour_idx >= g.end_pts.len() {
        return (0, 0);
    }
    let end = g.end_pts[contour_idx] as usize;
    let start = if contour_idx == 0 { 0 } else { g.end_pts[contour_idx - 1] as usize + 1 };
    (start, end)
}

#[inline]
fn jump_rel(pc: usize, off: i32) -> usize {
    if off >= 0 { pc + off as usize } else { pc.saturating_sub((-off) as usize) }
}

/// SROUND/S45ROUND 编码 -> 舍入三元组。
fn decode_round(enc: i32) -> RoundState {
    let period_code = (enc >> 6) & 0x3;
    let phase_code = (enc >> 4) & 0x3;
    let threshold_code = enc & 0xF;
    let period = match period_code {
        0 => F26Dot6(1),
        1 => F26Dot6(32),
        2 => F26Dot6::ONE,
        _ => F26Dot6(2 * 64),
    };
    let phase = match phase_code {
        0 => F26Dot6::ZERO,
        1 => F26Dot6(16),
        2 => F26Dot6::HALF,
        _ => F26Dot6(48),
    };
    let threshold_raw = ((threshold_code - 4) as i64 * period.raw() as i64) / 8;
    RoundState { period, phase, threshold: F26Dot6(threshold_raw as i32) }
}
