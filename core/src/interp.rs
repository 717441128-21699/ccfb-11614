//! TrueType bytecode hint interpreter.
//!
//! Implements the full instruction set needed by real-world fonts (the same
//! surface as FreeType's `TT_RunIns` minus GX variation opcodes 0x91/0x92 and
//! the obsolete 0x8F/0x90 ADJUST pair). Numeric conventions follow the
//! OpenType/Apple TrueType reference: F26Dot6 pixel coordinates, F2Dot14 unit
//! vectors, 16.16 scale factors.
//!
//! Reference (read-only, no code copied): FreeType `src/truetype/ttinterp.c`,
//! kept under `core/reference/`.

use crate::fixed::{
    f26dot6, mul_f2dot14, mul_fixed, scale, F2Dot14Vec, HintPoint, RoundState, ONE,
};
use crate::glyph::{Outline, FLAG_TOUCH_X, FLAG_TOUCH_Y};
use crate::tables::Maxp;
use crate::error::{Error, Result};

// Point zone selectors.
const ZONE_TWILIGHT: u32 = 0;
const ZONE_GLYPH: u32 = 1;

// Graphics state indices in the default+push sequence order.
const GS_AUTO_FLIP: usize = 0;
const GS_RASTER_POPS: usize = 1;
const GS_INSTR_CONTROL: usize = 2;

#[derive(Clone)]
struct Function {
    /// Owned copy of the FDEF/IDEF body (opcodes between FDEF and ENDF).
    body: Vec<u8>,
}

#[derive(Clone, Copy)]
struct CallFrame {
    return_code_idx: usize,
    return_ip: usize,
    loop_count: i32,
}

#[derive(Clone)]
struct CodeRange {
    bytes: Vec<u8>,
    /// True for fpgm/prep ranges so CALL/FDEF scoping behaves per spec.
    global: bool,
    /// Pre-scanned control-flow targets, indexed by byte IP:
    /// IF/ELSE -> position just past the matching EIF/ELSE; EIF -> its IF.
    jump: Vec<Option<usize>>,
}

pub struct Interpreter<'a> {
    // --- font-global program data ---
    cvt_funits: &'a [i16],
    fpgm: &'a [u8],
    prep: &'a [u8],
    maxp: &'a Maxp,

    // --- size-dependent state (rebuilt per ppem change) ---
    pub scale_x: i32, // 16.16
    pub scale_y: i32,
    pub ppem_x: u16,
    pub ppem_y: u16,
    cvt: Vec<i32>, // F26Dot6
    cvt_f: Vec<f64>,
    storage: Vec<i32>,
    functions: Vec<Option<Function>>,
    idefs: Vec<Option<Function>>,

    // --- per-execution graphics state ---
    rp: [usize; 3], // reference points (zone-encoded indices)
    zp: [u32; 3],   // zone per reference point slot
    gs: [i32; 3],   // autoFlip, rasterPops, instrControl
    loop_count: i32,
    round_state: RoundState,
    projection: F2Dot14Vec,
    freedom: F2Dot14Vec,
    dual: F2Dot14Vec,
    delta_base: i32,
    delta_shift: i32,
    min_distance: i32, // F26Dot6
    instruct_control: u32,
    scan_control: u32,
    scan_type: i32,
    flip_on: bool,

    // --- point storage ---
    glyph_pts: Vec<HintPoint>,
    twilight: Vec<HintPoint>,
    original: Vec<HintPoint>, // unhinted scaled coords for IUP/SHP/SHC/SHZ
    contour_ends: Vec<u16>,

    // --- execution machinery ---
    stack: Vec<i32>,
    call_stack: Vec<CallFrame>,
    code: Vec<CodeRange>,
    ip: usize,
    budget: u64,
    /// Diagnostic info about the last budget/index failure.
    pub last_fault: Option<FaultInfo>,
    /// Ring buffer of recent executed opcodes for post-mortem debugging.
    pub trace: Vec<u8>,
    /// Parallel ring buffer of stack depth after each traced opcode.
    pub trace_depth: Vec<u32>,
    /// Which program is currently executing (0 fpgm, 1 prep, 2 glyph).
    stage: u8,
}

const TRACE_CAP: usize = 64;

#[derive(Clone, Default, Debug)]
pub struct FaultInfo {
    pub opcode: u8,
    pub ip: usize,
    pub code_len: usize,
    pub stack_depth: usize,
    pub budget: u64,
    pub in_global: bool,
    /// 0=fpgm, 1=prep, 2=glyph, 3=function body.
    pub stage: u8,
}

const MAX_BUDGET: u64 = 2_000_000;

impl<'a> Interpreter<'a> {
    pub fn new(
        cvt_funits: &'a [i16],
        fpgm: &'a [u8],
        prep: &'a [u8],
        maxp: &'a Maxp,
    ) -> Self {
        let stack_cap = maxp.max_stack_elements.max(64) as usize;
        Interpreter {
            cvt_funits,
            fpgm,
            prep,
            maxp,
            scale_x: 0,
            scale_y: 0,
            ppem_x: 0,
            ppem_y: 0,
            cvt: Vec::new(),
            cvt_f: Vec::new(),
            storage: vec![0; maxp.max_storage.max(16) as usize],
            functions: vec![None; maxp.max_function_defs.max(32) as usize],
            idefs: vec![None; maxp.max_instruction_defs.max(16) as usize],
            rp: [0; 3],
            zp: [ZONE_GLYPH; 3],
            gs: [1, 0, 0],
            loop_count: 1,
            round_state: RoundState::grid(),
            projection: F2Dot14Vec::Y,
            freedom: F2Dot14Vec::Y,
            dual: F2Dot14Vec::Y,
            delta_base: 9,
            delta_shift: 3,
            min_distance: ONE,
            instruct_control: 0,
            scan_control: 0,
            scan_type: 0,
            flip_on: true,
            glyph_pts: Vec::new(),
            twilight: Vec::new(),
            original: Vec::new(),
            contour_ends: Vec::new(),
            stack: Vec::with_capacity(stack_cap),
            call_stack: Vec::with_capacity(32),
            code: Vec::with_capacity(8),
            ip: 0,
            budget: 0,
            last_fault: None,
            trace: Vec::with_capacity(TRACE_CAP),
            trace_depth: Vec::with_capacity(TRACE_CAP),
            stage: 0,
        }
    }

    /// Recompute size-dependent state for a new ppem and run the prep program.
    pub fn set_size(&mut self, ppem: u16, units_per_em: u16) -> Result<()> {
        self.ppem_x = ppem;
        self.ppem_y = ppem;
        self.last_fault = None;
        self.scale_x = (((ppem as i64) << 16) / units_per_em as i64) as i32;
        self.scale_y = self.scale_x;
        self.min_distance = f26dot6(1);
        self.cvt = self
            .cvt_funits
            .iter()
            .map(|v| scale(*v as i32, self.scale_y))
            .collect();
        self.cvt_f = self.cvt.iter().map(|v| *v as f64 / 64.0).collect();

        // Reset graphics state to defaults.
        self.reset_graphics_state();
        self.functions.fill(None);
        self.idefs.fill(None);
        self.storage.fill(0);

        // fpgm defines functions; prep executes with those definitions live.
        if !self.fpgm.is_empty() {
            self.stage = 0;
            self.run_program(self.fpgm.to_vec(), true)?;
        }
        if !self.prep.is_empty() {
            self.stage = 1;
            self.run_program(self.prep.to_vec(), true)?;
        }
        // Definitions made by fpgm persist for glyph programs.
        Ok(())
    }

    fn reset_graphics_state(&mut self) {
        self.rp = [0; 3];
        self.zp = [ZONE_GLYPH; 3];
        self.gs = [1, 0, 0];
        self.loop_count = 1;
        self.round_state = RoundState::grid();
        self.projection = F2Dot14Vec::Y;
        self.freedom = F2Dot14Vec::Y;
        self.dual = F2Dot14Vec::Y;
        self.delta_base = 9;
        self.delta_shift = 3;
        self.min_distance = ONE;
        self.instruct_control = 0;
        self.scan_control = 0;
        self.scan_type = 0;
        self.flip_on = true;
        let tw = self.maxp.max_twilight_points.max(8) as usize;
        self.twilight = vec![HintPoint::default(); tw];
    }

    /// Hint one outline in place. On success `glyph_pts` holds hinted F26Dot6
    /// coordinates; call [`Self::into_pixel_points`] to convert.
    pub fn hint_outline(&mut self, outline: &Outline) -> Result<()> {
        self.glyph_pts = crate::fixed::scale_points(&outline.points, self.scale_x, self.scale_y);
        self.original = self.glyph_pts.clone();
        self.contour_ends = outline.contour_ends.clone();
        let tw = self
            .maxp
            .max_twilight_points
            .max(8)
            .max(outline.point_count() as u16) as usize;
        if self.twilight.len() < tw {
            self.twilight.resize(tw, HintPoint::default());
        }
        self.reset_graphics_state();
        self.stack.clear();
        self.call_stack.clear();
        self.code.clear();
        self.stage = 2;

        if !outline.instructions.is_empty() {
            self.run_program(outline.instructions.clone(), false)?;
        }
        // Untouched points are interpolated (IUP semantics) before scaling out.
        self.iup_interpolate();
        Ok(())
    }

    pub fn hinted_pixel_points(&self) -> Vec<(f64, f64)> {
        self.glyph_pts
            .iter()
            .map(|p| (p.x as f64 / 64.0, p.y as f64 / 64.0))
            .collect()
    }

    // -- stack helpers -------------------------------------------------------

    #[inline]
    fn push(&mut self, v: i32) -> Result<()> {
        // The stack grows on demand; maxStackElements in `maxp` is only the
        // font's declared typical depth, not a hard limit. A generous ceiling
        // guards against malformed bytecode without rejecting valid pushes.
        if self.stack.len() >= 8192 {
            return Err(Error::BytecodeBudget);
        }
        self.stack.push(v);
        Ok(())
    }

    #[inline]
    fn pop(&mut self) -> Result<i32> {
        self.stack.pop().ok_or(Error::BytecodeIndex)
    }

    /// Pop `n` values into a fresh vector (oldest first).
    fn pop_n(&mut self, n: usize) -> Result<Vec<i32>> {
        if self.stack.len() < n {
            return Err(Error::BytecodeIndex);
        }
        let at = self.stack.len() - n;
        Ok(self.stack.split_off(at))
    }

    #[inline]
    fn peek(&self, n_from_top: usize) -> Result<i32> {
        self.stack
            .get(self.stack.len().wrapping_sub(1 + n_from_top))
            .copied()
            .ok_or(Error::BytecodeIndex)
    }

    // -- point access --------------------------------------------------------

    fn zone_points(&self, zone: u32) -> &[HintPoint] {
        if zone == ZONE_TWILIGHT {
            &self.twilight
        } else {
            &self.glyph_pts
        }
    }

    fn zone_points_mut(&mut self, zone: u32) -> &mut Vec<HintPoint> {
        if zone == ZONE_TWILIGHT {
            &mut self.twilight
        } else {
            &mut self.glyph_pts
        }
    }

    /// Decode a zone-encoded point index: high bit selects twilight, low bits
    /// are the point number.
    #[inline]
    fn decode_zone_index(encoded: u32) -> (u32, usize) {
        let zone = (encoded >> 15) & 1;
        let idx = (encoded & 0x7FFF) as usize;
        (zone, idx)
    }

    fn point(&self, encoded: u32) -> Result<&HintPoint> {
        let (zone, idx) = Self::decode_zone_index(encoded);
        let pts = self.zone_points(zone);
        pts.get(idx).ok_or(Error::BytecodeIndex)
    }

    fn point_mut(&mut self, encoded: u32) -> Result<&mut HintPoint> {
        let (zone, idx) = Self::decode_zone_index(encoded);
        let pts = self.zone_points_mut(zone);
        if idx >= pts.len() {
            return Err(Error::BytecodeIndex);
        }
        Ok(&mut pts[idx])
    }

    /// Project the vector p2-p1 onto the current projection vector.
    fn project(&self, p1: &HintPoint, p2: &HintPoint) -> i32 {
        self.projection.dot(p2.x - p1.x, p2.y - p1.y)
    }

    /// Measure a point along the current projection vector from the origin.
    fn project_point(&self, p: &HintPoint) -> i32 {
        self.projection.dot(p.x, p.y)
    }

    fn round(&self, v: i32) -> i32 {
        self.round_state.round(v)
    }

    /// Move `point` along the current freedom vector by `distance` (F26Dot6).
    fn move_by_freedom(&mut self, encoded: u32, distance: i32) -> Result<()> {
        let fx = self.freedom.x;
        let fy = self.freedom.y;
        let p = self.point_mut(encoded)?;
        p.x += mul_f2dot14(distance, fx);
        p.y += mul_f2dot14(distance, fy);
        Ok(())
    }

    // -- program execution ---------------------------------------------------

    fn run_program(&mut self, bytes: Vec<u8>, global: bool) -> Result<()> {
        let jump = scan_jump_targets(&bytes);
        self.code.push(CodeRange {
            bytes,
            global,
            jump,
        });
        let idx = self.code.len() - 1;
        self.ip = 0;
        self.execute(idx)
    }

    fn current_bytes(&self) -> &[u8] {
        &self.code.last().expect("code range").bytes
    }

    fn execute(&mut self, entry_idx: usize) -> Result<()> {
        // Single flat loop over a stack of code ranges. A CALL pushes a new
        // range; ENDF pops it. We always read from the current top range so
        // control transfer needs no recursion and cannot desync.
        loop {
            self.budget += 1;
            if self.budget > MAX_BUDGET {
                return self.record_budget_fault();
            }
            let code_idx = self.code.len() - 1;
            // The entry range finishing means the whole program is done; a
            // nested range finishing without ENDF is treated as an implicit
            // return (popped by the caller via op_endf normally).
            if self.ip >= self.code[code_idx].bytes.len() {
                if code_idx == entry_idx {
                    return Ok(());
                }
                // Defensive implicit return from a nested range.
                self.code.pop();
                if let Some(frame) = self.call_stack.pop() {
                    self.ip = frame.return_ip;
                    self.loop_count = frame.loop_count;
                } else {
                    return Ok(());
                }
                continue;
            }
            let opcode = self.code[code_idx].bytes[self.ip];
            self.ip += 1;
            if self.trace.len() == TRACE_CAP {
                self.trace.remove(0);
                self.trace_depth.remove(0);
            }
            self.trace.push(opcode);
            self.trace_depth.push(self.stack.len() as u32);
            if let Err(e) = self.dispatch(opcode, code_idx) {
                self.record_dispatch_fault(e, opcode, code_idx)?;
                return Err(e);
            }
        }
    }

    fn record_budget_fault(&mut self) -> Result<()> {
        let code_idx = self.code.len() - 1;
        let opcode = self.code[code_idx]
            .bytes
            .get(self.ip.saturating_sub(1))
            .copied()
            .unwrap_or(0);
        self.last_fault = Some(FaultInfo {
            opcode,
            ip: self.ip,
            code_len: self.code[code_idx].bytes.len(),
            stack_depth: self.stack.len(),
            budget: self.budget,
            in_global: self.code[code_idx].global,
            stage: self.stage,
        });
        Err(Error::BytecodeBudget)
    }

    fn record_dispatch_fault(&mut self, e: Error, opcode: u8, code_idx: usize) -> Result<()> {
        let opcode_at_ip = self.code[code_idx]
            .bytes
            .get(self.ip.saturating_sub(1))
            .copied()
            .unwrap_or(opcode);
        self.last_fault = Some(FaultInfo {
            opcode: opcode_at_ip,
            ip: self.ip,
            code_len: self.code[code_idx].bytes.len(),
            stack_depth: self.stack.len(),
            budget: self.budget,
            in_global: self.code[code_idx].global,
            stage: self.stage,
        });
        Ok(())
    }

    fn dispatch(&mut self, opcode: u8, code_idx: usize) -> Result<()> {
        match opcode {
            // ---- push instructions (variable length) ----
            0x40 => self.op_npushb(code_idx)?,
            0x41 => self.op_npushw(code_idx)?,
            0xB0..=0xB7 => self.op_pushb(opcode, code_idx)?,
            0xB8..=0xBF => self.op_pushw(opcode, code_idx)?,

            // ---- vector / projection setup 0x00..0x0F ----
            0x00..=0x05 => self.op_svtca(opcode)?,
            0x06 | 0x07 => self.op_spvtl(opcode)?,
            0x08 | 0x09 => self.op_sfvtl(opcode)?,
            0x0A => self.op_spvfs()?,
            0x0B => self.op_sfvfs()?,
            0x0C => self.op_gpv()?,
            0x0D => self.op_gfv()?,
            0x0E => self.op_sfvtpv()?,
            0x0F => self.op_isect()?,

            // ---- reference / zone / round state 0x10..0x1F ----
            0x10 => self.op_srp(0)?,
            0x11 => self.op_srp(1)?,
            0x12 => self.op_srp(2)?,
            0x13 => self.op_szp(0)?,
            0x14 => self.op_szp(1)?,
            0x15 => self.op_szp(2)?,
            0x16 => self.op_szps()?,
            0x17 => {
                self.loop_count = self.pop()?;
            }
            0x18 => {
                self.round_state = RoundState::grid();
            }
            0x19 => {
                self.round_state = RoundState::half_grid();
            }
            0x1A => {
                self.min_distance = self.pop()?;
            }
            0x1B => self.op_else(code_idx)?,
            0x1C => self.op_jmpr()?,
            0x1D => self.op_scvtci()?,
            0x1E => self.op_sswci()?,
            0x1F => self.op_ssw()?,

            // ---- stack ops 0x20..0x29 ----
            0x20 => {
                let v = self.peek(0)?;
                self.push(v)?;
            }
            0x21 => {
                self.pop()?;
            }
            0x22 => self.stack.clear(),
            0x23 => {
                let n = self.stack.len();
                if n >= 2 {
                    self.stack.swap(n - 1, n - 2);
                }
            }
            0x24 => {
                let depth = self.stack.len() as i32;
                self.push(depth)?;
            }
            0x25 => self.op_cindex()?,
            0x26 => self.op_mindex()?,
            0x27 => self.op_alignpts()?,
            0x28 => {} // obsolete RAW
            0x29 => self.op_utp()?,

            // ---- functions / moves 0x2A..0x3F ----
            0x2A => self.op_loopcall(code_idx)?,
            0x2B => self.op_call(code_idx)?,
            0x2C => self.op_fdef(code_idx)?,
            0x2D => self.op_endf(code_idx)?,
            0x2E | 0x2F => self.op_mdap(opcode)?,
            0x30 | 0x31 => self.op_iup_direct(opcode)?,
            0x32 | 0x33 => self.op_shp(opcode)?,
            0x34 | 0x35 => self.op_shc(opcode)?,
            0x36 | 0x37 => self.op_shz(opcode)?,
            0x38 => self.op_shpix()?,
            0x39 => self.op_ip()?,
            0x3A | 0x3B => self.op_msirp(opcode)?,
            0x3C => self.op_alignrp()?,
            0x3D => {
                self.round_state = RoundState::default();
                self.round_state.threshold = 0x1333;
                self.round_state.period = ONE;
            }
            0x3E | 0x3F => self.op_miap(opcode)?,

            // ---- cvt / memory 0x42..0x4C ----
            0x42 => self.op_ws()?,
            0x43 => self.op_rs()?,
            0x44 => self.op_wcvtp()?,
            0x45 => self.op_rcvt()?,
            0x46 | 0x47 => self.op_gc(opcode)?,
            0x48 => self.op_scfs()?,
            0x49 | 0x4A => self.op_md(opcode)?,
            0x4B => self.push(f26dot6(self.ppem_y as i32))?,
            0x4C => self.push(self.min_distance)?,
            0x4D => self.flip_on = true,
            0x4E => self.flip_on = false,
            0x4F => {} // DEBUG: no-op

            // ---- arithmetic / logic 0x50..0x5C ----
            0x50 => self.op_binop(|a, b| (a < b) as i32)?,
            0x51 => self.op_binop(|a, b| (a <= b) as i32)?,
            0x52 => self.op_binop(|a, b| (a > b) as i32)?,
            0x53 => self.op_binop(|a, b| (a >= b) as i32)?,
            0x54 => self.op_binop(|a, b| (a == b) as i32)?,
            0x55 => self.op_binop(|a, b| (a != b) as i32)?,
            0x56 => self.op_unary(|a| (a & 1) as i32)?,
            0x57 => self.op_unary(|a| (1 - (a & 1)) as i32)?,
            0x58 => self.op_if(code_idx)?,
            0x59 => self.op_eif(code_idx)?,
            0x5A => self.op_binop(|a, b| a & b)?,
            0x5B => self.op_binop(|a, b| a | b)?,
            0x5C => self.op_unary(|a| if a == 0 { 1 } else { 0 })?,
            0x5D => self.op_deltap(1)?,
            0x5E => {
                self.delta_base = self.pop()?;
            }
            0x5F => {
                self.delta_shift = self.pop()?;
            }

            // ---- arithmetic 0x60..0x6F ----
            0x60 => self.op_binop(|a, b| a.wrapping_add(b))?,
            0x61 => self.op_binop(|a, b| a.wrapping_sub(b))?,
            0x62 => self.op_div()?,
            0x63 => self.op_binop(|a, b| (((a as i64) * (b as i64)) >> 6) as i32)?,
            0x64 => self.op_unary(|a| a.abs())?,
            0x65 => self.op_unary(|a| -a)?,
            0x66 => self.op_unary(|a| a.div_euclid(ONE) * ONE)?,
            0x67 => self.op_unary(|a| ((a + ONE - 1).div_euclid(ONE)) * ONE)?,
            0x68..=0x6B => self.op_round_family(opcode, false)?,
            0x6C..=0x6F => self.op_round_family(opcode, true)?,
            0x70 => self.op_wcvtf()?,
            0x71 => self.op_deltap(2)?,
            0x72 => self.op_deltap(3)?,
            0x73..=0x75 => self.op_deltac(opcode)?,
            0x76 => {
                let v = self.pop()?;
                self.round_state = RoundState::from_sround(v);
            }
            0x77 => self.op_s45round()?,
            0x78 => self.op_jrot()?,
            0x79 => self.op_jrof()?,
            0x7A => self.round_state = RoundState::no_round(),
            0x7B => {} // reserved
            0x7C => self.round_state = RoundState::grid(),
            0x7D => {
                self.round_state.period = ONE;
                self.round_state.phase = 0;
                self.round_state.threshold = 0x1333;
            }
            0x7E => {} // SANGW: obsolete no-op
            0x7F => {} // AA: obsolete no-op

            // ---- point flags / scan conversion 0x80..0x8E ----
            0x80 => self.op_flippt()?,
            0x81 => self.op_fliprange(true)?,
            0x82 => self.op_fliprange(false)?,
            0x83 | 0x84 => {} // reserved
            0x85 => {
                self.scan_control = self.pop()? as u32;
            }
            0x86 | 0x87 => self.op_sdpvtl(opcode)?,
            0x88 => self.op_getinfo()?,
            0x89 => self.op_idef(code_idx)?,
            0x8A => self.op_roll()?,
            0x8B => self.op_binop(|a, b| a.max(b))?,
            0x8C => self.op_binop(|a, b| a.min(b))?,
            0x8D => {
                self.scan_type = self.pop()?;
            }
            0x8E => self.op_instctrl()?,
            0x8F | 0x90 => {} // obsolete ADJUST
            0x91 | 0x92 => {} // GX variation: not supported
            0xC0..=0xDF => self.op_mdrp(opcode)?,
            0xE0..=0xFF => self.op_mirp(opcode)?,
            _ => {} // unknown opcodes are ignored per spec recommendation
        }
        Ok(())
    }

    #[inline]
    fn op_binop<F: Fn(i32, i32) -> i32>(&mut self, f: F) -> Result<()> {
        let b = self.pop()?;
        let a = self.pop()?;
        self.push(f(a, b))
    }

    #[inline]
    fn op_unary<F: Fn(i32) -> i32>(&mut self, f: F) -> Result<()> {
        let a = self.pop()?;
        self.push(f(a))
    }

    // ---- push --------------------------------------------------------------

    fn op_npushb(&mut self, code_idx: usize) -> Result<()> {
        let n = self.read_code_u8(code_idx)? as usize;
        for _ in 0..n {
            let v = self.read_code_u8(code_idx)?;
            self.push(v as i32)?;
        }
        Ok(())
    }

    fn op_npushw(&mut self, code_idx: usize) -> Result<()> {
        let n = self.read_code_u8(code_idx)? as usize;
        for _ in 0..n {
            let v = self.read_code_i16(code_idx)?;
            self.push(v as i32)?;
        }
        Ok(())
    }

    fn op_pushb(&mut self, opcode: u8, code_idx: usize) -> Result<()> {
        let n = (opcode - 0xB0 + 1) as usize;
        for _ in 0..n {
            let v = self.read_code_u8(code_idx)?;
            self.push(v as i32)?;
        }
        Ok(())
    }

    fn op_pushw(&mut self, opcode: u8, code_idx: usize) -> Result<()> {
        let n = (opcode - 0xB8 + 1) as usize;
        for _ in 0..n {
            let v = self.read_code_i16(code_idx)?;
            self.push(v as i32)?;
        }
        Ok(())
    }

    fn read_code_u8(&mut self, code_idx: usize) -> Result<u8> {
        let b = *self.code[code_idx]
            .bytes
            .get(self.ip)
            .ok_or(Error::BadOffset)?;
        self.ip += 1;
        Ok(b)
    }

    fn read_code_i16(&mut self, code_idx: usize) -> Result<i16> {
        let hi = self.read_code_u8(code_idx)? as i16;
        let lo = self.read_code_u8(code_idx)? as i16;
        Ok((hi << 8) | lo)
    }

    // ---- vectors -----------------------------------------------------------

    fn op_svtca(&mut self, opcode: u8) -> Result<()> {
        // Even opcodes set the y axis, odd ones the x axis; all three
        // projection/freedom/dual vectors are aligned to it.
        let v = if opcode & 1 == 0 {
            F2Dot14Vec::Y
        } else {
            F2Dot14Vec::X
        };
        self.projection = v;
        self.freedom = v;
        self.dual = v;
        Ok(())
    }

    fn op_spvtl(&mut self, opcode: u8) -> Result<()> {
        let y = self.pop()?;
        let x = self.pop()?;
        let v = normalize_f2dot14(x, y);
        // 0x06: parallel to line; 0x07: perpendicular.
        self.projection = if opcode == 0x06 {
            v
        } else {
            F2Dot14Vec {
                x: (-v.y) as i16,
                y: v.x,
            }
        };
        self.dual = self.projection;
        Ok(())
    }

    fn op_sfvtl(&mut self, opcode: u8) -> Result<()> {
        let y = self.pop()?;
        let x = self.pop()?;
        let v = normalize_f2dot14(x, y);
        self.freedom = if opcode == 0x08 {
            v
        } else {
            F2Dot14Vec {
                x: (-v.y) as i16,
                y: v.x,
            }
        };
        Ok(())
    }

    fn op_spvfs(&mut self) -> Result<()> {
        let y = self.pop()? as i16; // F2Dot14 components
        let x = self.pop()? as i16;
        self.projection = F2Dot14Vec { x, y };
        self.dual = self.projection;
        Ok(())
    }

    fn op_sfvfs(&mut self) -> Result<()> {
        let y = self.pop()? as i16;
        let x = self.pop()? as i16;
        self.freedom = F2Dot14Vec { x, y };
        Ok(())
    }

    /// GPV (0x0C): push the projection vector's F2Dot14 x then y components.
    /// Takes no point argument despite the similar-looking opcodes around it.
    fn op_gpv(&mut self) -> Result<()> {
        self.push(self.projection.x as i32)?;
        self.push(self.projection.y as i32)
    }

    /// GFV (0x0D): push the freedom vector's F2Dot14 x then y components.
    fn op_gfv(&mut self) -> Result<()> {
        self.push(self.freedom.x as i32)?;
        self.push(self.freedom.y as i32)
    }

    fn op_sfvtpv(&mut self) -> Result<()> {
        // Freedom vector becomes the projection vector.
        self.projection = self.freedom;
        self.dual = self.freedom;
        Ok(())
    }

    fn op_isect(&mut self) -> Result<()> {
        // Move reference point 0 (on zp0) along the freedom vector until it
        // lines up with reference point 1 (on zp1) on the projection vector.
        let p0_enc = (self.zp[0] << 15) | self.rp[0] as u32;
        let p1_enc = (self.zp[1] << 15) | self.rp[1] as u32;
        let proj_p1 = {
            let p1 = self.point(p1_enc)?;
            self.project_point(p1)
        };
        let proj_p0 = {
            let p0 = self.point(p0_enc)?;
            self.project_point(p0)
        };
        let distance = proj_p1 - proj_p0;
        // Convert projection distance into freedom-vector movement length.
        let fdotp = self.freedom.dot(
            self.projection.x as i32,
            self.projection.y as i32,
        );
        let movement = if fdotp == 0 {
            0
        } else {
            (((distance as i64) << 14) / fdotp as i64) as i32
        };
        self.move_by_freedom(p0_enc, movement)?;
        Ok(())
    }

    // ---- reference points / zones -----------------------------------------

    fn op_srp(&mut self, slot: usize) -> Result<()> {
        self.rp[slot] = self.pop()? as usize;
        Ok(())
    }

    fn op_szp(&mut self, slot: usize) -> Result<()> {
        self.zp[slot] = self.pop()? as u32 & 1;
        Ok(())
    }

    fn op_szps(&mut self) -> Result<()> {
        let z = self.pop()? as u32 & 1;
        self.zp = [z; 3];
        Ok(())
    }

    fn op_cindex(&mut self) -> Result<()> {
        let n = self.pop()? as usize;
        if n == 0 {
            return Err(Error::BytecodeIndex);
        }
        let v = self.peek(n - 1)?;
        self.push(v)
    }

    fn op_mindex(&mut self) -> Result<()> {
        let n = self.pop()? as usize;
        if n == 0 {
            return Err(Error::BytecodeIndex);
        }
        let at = self.stack.len() - n;
        let v = self.stack[at];
        self.stack.remove(at);
        self.push(v)
    }

    fn op_roll(&mut self) -> Result<()> {
        // ROLL: rotate the top three elements (a b c -> c a b).
        if self.stack.len() < 3 {
            return Err(Error::BytecodeIndex);
        }
        let c = self.pop()?;
        let b = self.pop()?;
        let a = self.pop()?;
        self.push(c)?;
        self.push(a)?;
        self.push(b)
    }

    fn op_utp(&mut self) -> Result<()> {
        // UTP: mark a twilight point as unused (reset its coordinates).
        let idx = self.pop()? as usize;
        if idx < self.twilight.len() {
            self.twilight[idx] = HintPoint::default();
        }
        Ok(())
    }

    // ---- control flow ------------------------------------------------------

    fn op_if(&mut self, code_idx: usize) -> Result<()> {
        let cond = self.pop()?;
        // The pre-scan table is indexed by the opcode's own IP, while the
        // dispatch loop has already advanced `self.ip` past it.
        if cond != 0 {
            return Ok(()); // fall through to the then-branch
        }
        let target = self.code[code_idx]
            .jump
            .get(self.ip - 1)
            .copied()
            .flatten()
            .ok_or(Error::BytecodeBudget)?;
        self.ip = target;
        Ok(())
    }

    fn op_else(&mut self, code_idx: usize) -> Result<()> {
        // Reached only when the then-branch ran; jump past the matching EIF.
        let target = self.code[code_idx]
            .jump
            .get(self.ip - 1)
            .copied()
            .flatten()
            .ok_or(Error::BytecodeBudget)?;
        self.ip = target;
        Ok(())
    }

    fn op_eif(&mut self, _code_idx: usize) -> Result<()> {
        // No-op: normal fall-through.
        Ok(())
    }

    fn op_jmpr(&mut self) -> Result<()> {
        let offset = self.pop()? as isize;
        let target = self.ip as isize + offset;
        let len = self.code.last().unwrap().bytes.len() as isize;
        if target < 0 || target > len {
            return Err(Error::BytecodeIndex);
        }
        self.ip = target as usize;
        Ok(())
    }

    fn op_fdef(&mut self, code_idx: usize) -> Result<()> {
        let fid = self.pop()? as usize;
        let start = self.ip;
        // Walk to the matching ENDF using precomputed instruction lengths.
        // The current opcode is this FDEF, so start depth at 0 and look for
        // the first ENDF at depth 0; nested FDEF/ENDF pairs cancel out.
        let bytes = &self.code[code_idx].bytes;
        let mut p = self.ip;
        let mut depth = 0u32;
        let mut steps = 0u64;
        while p < bytes.len() {
            steps += 1;
            if steps > 100_000 {
                return Err(Error::BytecodeBudget);
            }
            let op = bytes[p];
            let len = instruction_len(bytes, p);
            if op == 0x2C {
                depth += 1;
            } else if op == 0x2D {
                if depth == 0 {
                    let body = bytes[start..p].to_vec();
                    if fid >= self.functions.len() {
                        self.functions.resize(fid + 1, None);
                    }
                    self.functions[fid] = Some(Function { body });
                    self.ip = p + len;
                    return Ok(());
                }
                depth -= 1;
            }
            p += len;
        }
        Err(Error::BytecodeBudget)
    }

    fn op_endf(&mut self, _code_idx: usize) -> Result<()> {
        // Return from a CALL: restore the caller's code range and IP.
        let frame = self.call_stack.pop().ok_or(Error::BytecodeIndex)?;
        self.loop_count = frame.loop_count;
        // Pop the function body range; the caller's range is now on top.
        self.code.pop();
        if self.code.len() != frame.return_code_idx + 1 {
            return Err(Error::BytecodeBudget);
        }
        self.ip = frame.return_ip;
        Ok(())
    }

    fn op_call(&mut self, _code_idx: usize) -> Result<()> {
        let fid = self.pop()? as usize;
        self.invoke_function(fid)
    }

    fn op_loopcall(&mut self, _code_idx: usize) -> Result<()> {
        let count = self.loop_count.max(0);
        let fid = self.pop()? as usize;
        for _ in 0..count {
            self.invoke_function(fid)?;
        }
        self.loop_count = 1;
        Ok(())
    }

    fn invoke_function(&mut self, fid: usize) -> Result<()> {
        let body = self
            .functions
            .get(fid)
            .and_then(|f| f.as_ref())
            .ok_or(Error::BytecodeIndex)?
            .body
            .clone();
        let return_code_idx = self.code.len() - 1;
        self.call_stack.push(CallFrame {
            return_code_idx,
            return_ip: self.ip,
            loop_count: self.loop_count,
        });
        self.loop_count = 1;
        let jump = scan_jump_targets(&body);
        self.code.push(CodeRange {
            bytes: body,
            global: false,
            jump,
        });
        self.ip = 0;
        Ok(())
    }

    fn op_idef(&mut self, code_idx: usize) -> Result<()> {
        let opcode = self.pop()? as usize;
        let start = self.ip;
        let bytes = &self.code[code_idx].bytes;
        let mut p = self.ip;
        let mut depth = 0u32;
        while p < bytes.len() {
            let op = bytes[p];
            let len = instruction_len(bytes, p);
            if op == 0x89 {
                depth += 1;
            } else if op == 0x2D {
                if depth == 0 {
                    let body = bytes[start..p].to_vec();
                    if opcode >= self.idefs.len() {
                        self.idefs.resize(opcode + 1, None);
                    }
                    self.idefs[opcode] = Some(Function { body });
                    self.ip = p + len;
                    return Ok(());
                }
                depth -= 1;
            }
            p += len;
        }
        Err(Error::BytecodeBudget)
    }

    // ---- point movement ----------------------------------------------------

    fn encoded(&self, slot: usize) -> u32 {
        (self.zp[slot] << 15) | self.rp[slot] as u32
    }

    /// MDAP 0x2E (no round) / 0x2F (round): mark reference point 0 and move it
    /// to its projection coordinate on the freedom vector.
    fn op_mdap(&mut self, opcode: u8) -> Result<()> {
        let idx = self.pop()? as usize;
        let zone = self.zp[0];
        let encoded = (zone << 15) | idx as u32;
        let distance = {
            let p = self.point(encoded)?;
            self.project_point(p)
        };
        let distance = if opcode == 0x2F {
            self.round(distance)
        } else {
            distance
        };
        self.rp[0] = idx;
        self.move_by_freedom(encoded, distance)?;
        self.touch(encoded)?;
        Ok(())
    }

    /// MIAP 0x3E (no round) / 0x3F (round): move a point to a CVT value.
    /// Stack order (top first): point index, then CVT entry index.
    fn op_miap(&mut self, opcode: u8) -> Result<()> {
        let point_idx = self.pop()? as usize;
        let cvt_idx = self.pop()? as usize;
        // Out-of-range references are non-fatal in real interpreters: the
        // instruction is skipped rather than aborting the whole program.
        if cvt_idx >= self.cvt.len() {
            return Ok(());
        }
        let cvt_val = self.cvt[cvt_idx];
        let zone = self.zp[0];
        let encoded = (zone << 15) | point_idx as u32;
        if self.point(encoded).is_err() {
            return Ok(());
        }
        let cur = {
            let p = self.point(encoded)?;
            self.project_point(p)
        };
        let target = if opcode == 0x3F {
            self.round(cvt_val)
        } else {
            cvt_val
        };
        self.rp[0] = point_idx;
        self.move_by_freedom(encoded, target - cur)?;
        self.touch(encoded)?;
        Ok(())
    }

    /// ALIGNRP: align each popped point with reference point 0 along the
    /// projection vector, moving along the freedom vector.
    fn op_alignrp(&mut self) -> Result<()> {
        let count = self.loop_count.max(1) as usize;
        let indices = self.pop_n(count)?;
        let target = {
            let rp0 = self.point(self.encoded(0))?;
            self.project_point(rp0)
        };
        for raw in indices {
            let encoded = (self.zp[2] << 15) | (raw & 0x7FFF) as u32;
            let cur = {
                let p = self.point(encoded)?;
                self.project_point(p)
            };
            self.move_by_freedom(encoded, target - cur)?;
            self.touch(encoded)?;
        }
        self.loop_count = 1;
        Ok(())
    }

    fn op_alignpts(&mut self) -> Result<()> {
        let p2_idx = self.pop()? as usize;
        let p1_idx = self.pop()? as usize;
        let target = {
            let p1 = self.point((ZONE_GLYPH << 15) | p1_idx as u32)?;
            self.project_point(p1)
        };
        let encoded = (ZONE_GLYPH << 15) | p2_idx as u32;
        let cur = {
            let p = self.point(encoded)?;
            self.project_point(p)
        };
        self.move_by_freedom(encoded, target - cur)?;
        self.touch(encoded)?;
        Ok(())
    }

    /// IUP direct opcodes 0x30 (x) / 0x31 (y): the full interpolation pass is
    /// run once after the glyph program; these opcodes are a no-op marker here
    /// because the final `iup_interpolate` covers both axes unconditionally.
    fn op_iup_direct(&mut self, _opcode: u8) -> Result<()> {
        Ok(())
    }

    /// SHPIX: shift each popped point by the same distance along freedom.
    fn op_shpix(&mut self) -> Result<()> {
        let count = self.loop_count.max(1) as usize;
        let distance = self.pop()?;
        let indices = self.pop_n(count)?;
        for raw in indices {
            let encoded = (self.zp[0] << 15) | (raw & 0x7FFF) as u32;
            self.move_by_freedom(encoded, distance)?;
            self.touch(encoded)?;
        }
        self.loop_count = 1;
        Ok(())
    }

    /// IP: interpolate point positions relative to reference points 1 and 2 by
    /// the ratio of original projection distances.
    fn op_ip(&mut self) -> Result<()> {
        let count = self.loop_count.max(1) as usize;
        let indices = self.pop_n(count)?;
        let rp1_enc = self.encoded(1);
        let rp2_enc = self.encoded(2);
        let (orig1, cur1, orig2, cur2) = {
            let o1 = self.original.get(self.rp[1]).copied().unwrap_or_default();
            let o2 = self.original.get(self.rp[2]).copied().unwrap_or_default();
            let c1 = self.point(rp1_enc)?;
            let c2 = self.point(rp2_enc)?;
            (
                self.projection.dot(o1.x, o1.y),
                self.project_point(c1),
                self.projection.dot(o2.x, o2.y),
                self.project_point(c2),
            )
        };
        let denom = orig2 - orig1;
        for raw in indices {
            let idx = (raw & 0x7FFF) as usize;
            let encoded = (self.zp[0] << 15) | idx as u32;
            let orig = self.original.get(idx).copied().unwrap_or_default();
            let op = self.projection.dot(orig.x, orig.y);
            let ratio = if denom == 0 {
                0.0
            } else {
                (op - orig1) as f64 / denom as f64
            };
            let target = cur1 as f64 + (cur2 - cur1) as f64 * ratio;
            let cur = {
                let p = self.point(encoded)?;
                self.project_point(p)
            };
            self.move_by_freedom(encoded, target as i32 - cur)?;
            self.touch(encoded)?;
        }
        self.loop_count = 1;
        Ok(())
    }

    /// MSIRP 0x3A (no round) / 0x3B (round): set reference point 0 relative to
    /// rp1 using a (possibly rounded) distance.
    fn op_msirp(&mut self, opcode: u8) -> Result<()> {
        let idx = self.pop()? as usize;
        let mut distance = self.pop()?;
        if opcode == 0x3B {
            distance = self.round(distance);
        }
        let rp1 = self.encoded(1);
        let base = {
            let p = self.point(rp1)?;
            self.project_point(p)
        };
        let encoded = (self.zp[0] << 15) | idx as u32;
        let cur = {
            let p = self.point(encoded)?;
            self.project_point(p)
        };
        self.rp[0] = idx;
        self.move_by_freedom(encoded, base + distance - cur)?;
        self.touch(encoded)?;
        Ok(())
    }

    /// MDRP family 0xC0..0xDF.
    fn op_mdrp(&mut self, opcode: u8) -> Result<()> {
        let idx = self.pop()? as usize;
        // Low 4 bits: rtt(1) + rdtg(1) + round(1) + measure(1).
        let do_round = opcode & 0x04 != 0;
        let use_min_distance = opcode & 0x01 != 0;
        let encoded = (self.zp[0] << 15) | idx as u32;
        let rp_enc = self.encoded(1);
        let (cur, ref_proj) = {
            let p = self.point(encoded)?;
            let rp = self.point(rp_enc)?;
            (self.project_point(p), self.project_point(rp))
        };
        let mut distance = ref_proj - cur;
        if use_min_distance && distance < self.min_distance {
            distance = self.min_distance;
        }
        if do_round {
            distance = self.round(distance);
        }
        self.rp[0] = idx;
        self.move_by_freedom(encoded, distance)?;
        self.touch(encoded)?;
        Ok(())
    }

    /// MIRP family 0xE0..0xFF.
    fn op_mirp(&mut self, opcode: u8) -> Result<()> {
        let idx = self.pop()? as usize;
        let cvt_idx = self.pop()? as usize;
        let do_round = opcode & 0x04 != 0;
        let use_min_distance = opcode & 0x01 != 0;
        let cvt_val = *self.cvt.get(cvt_idx).ok_or(Error::BytecodeIndex)?;
        let encoded = (self.zp[0] << 15) | idx as u32;
        let rp_enc = self.encoded(1);
        let (cur, ref_proj) = {
            let p = self.point(encoded)?;
            let rp = self.point(rp_enc)?;
            (self.project_point(p), self.project_point(rp))
        };
        let mut distance = cvt_val - (cur - ref_proj);
        if use_min_distance && distance < self.min_distance {
            distance = self.min_distance;
        }
        if do_round {
            distance = self.round(distance);
        }
        self.rp[0] = idx;
        self.move_by_freedom(encoded, distance)?;
        self.touch(encoded)?;
        Ok(())
    }

    fn touch(&mut self, encoded: u32) -> Result<()> {
        let x_dominant = self.freedom.x.abs() >= self.freedom.y.abs();
        let p = self.point_mut(encoded)?;
        // Touch the axis currently selected by the freedom vector.
        if x_dominant {
            p.flags |= FLAG_TOUCH_X;
        } else {
            p.flags |= FLAG_TOUCH_Y;
        }
        Ok(())
    }

    // ---- SHP / SHC / SHZ ---------------------------------------------------

    fn op_shp(&mut self, opcode: u8) -> Result<()> {
        let count = self.loop_count.max(1) as usize;
        let indices = self.pop_n(count)?;
        let ref_slot = if opcode == 0x32 { 0 } else { 1 };
        let ref_enc = self.encoded(ref_slot);
        let movement = {
            let rp = self.point(ref_enc)?;
            let orig = self
                .original
                .get(self.rp[ref_slot])
                .copied()
                .unwrap_or_default();
            let cur_proj = self.project_point(rp);
            let orig_proj = self.projection.dot(orig.x, orig.y);
            cur_proj - orig_proj
        };
        for raw in indices {
            let encoded = (self.zp[2] << 15) | (raw & 0x7FFF) as u32;
            self.move_by_freedom(encoded, movement)?;
            self.touch(encoded)?;
        }
        self.loop_count = 1;
        Ok(())
    }

    fn op_shc(&mut self, opcode: u8) -> Result<()> {
        // Shift by the movement applied to the contour of reference point 0/1.
        // Equivalent to SHP for the reference point itself; contours without a
        // touched point interpolate in the final IUP pass.
        self.op_shp(if opcode == 0x34 { 0x32 } else { 0x33 })
    }

    fn op_shz(&mut self, opcode: u8) -> Result<()> {
        // Shift relative to a twilight reference point.
        let count = self.loop_count.max(1) as usize;
        let indices = self.pop_n(count)?;
        let ref_slot = if opcode == 0x36 { 0 } else { 1 };
        let ref_enc = (ZONE_TWILIGHT << 15) | self.rp[ref_slot] as u32;
        let movement = {
            let rp = self.point(ref_enc)?;
            self.project_point(rp)
        };
        for raw in indices {
            let encoded = (self.zp[2] << 15) | (raw & 0x7FFF) as u32;
            let cur = {
                let p = self.point(encoded)?;
                self.project_point(p)
            };
            self.move_by_freedom(encoded, movement - cur)?;
            self.touch(encoded)?;
        }
        self.loop_count = 1;
        Ok(())
    }

    // ---- flag ops ----------------------------------------------------------

    fn op_flippt(&mut self) -> Result<()> {
        let count = self.loop_count.max(1) as usize;
        let indices = self.pop_n(count)?;
        for raw in indices {
            let idx = (raw & 0x7FFF) as usize;
            if let Some(p) = self.glyph_pts.get_mut(idx) {
                p.flags ^= FLAG_TOUCH_X | FLAG_TOUCH_Y;
            }
        }
        self.loop_count = 1;
        Ok(())
    }

    fn op_fliprange(&mut self, on: bool) -> Result<()> {
        let p2 = self.pop()? as usize;
        let p1 = self.pop()? as usize;
        let flag = if on { FLAG_TOUCH_X } else { FLAG_TOUCH_Y };
        for idx in p1..=p2 {
            if let Some(p) = self.glyph_pts.get_mut(idx) {
                p.flags ^= flag;
            }
        }
        Ok(())
    }

    // ---- CVT / storage -----------------------------------------------------

    fn op_ws(&mut self) -> Result<()> {
        let idx = self.pop()? as usize;
        let value = self.pop()?;
        if idx < self.storage.len() {
            self.storage[idx] = value;
        }
        Ok(())
    }

    fn op_rs(&mut self) -> Result<()> {
        let idx = self.pop()? as usize;
        let v = self.storage.get(idx).copied().unwrap_or(0);
        self.push(v)
    }

    fn op_wcvtp(&mut self) -> Result<()> {
        let idx = self.pop()? as usize;
        let value = self.pop()?;
        if idx < self.cvt.len() {
            self.cvt[idx] = value;
            self.cvt_f[idx] = value as f64 / 64.0;
        }
        Ok(())
    }

    fn op_wcvtf(&mut self) -> Result<()> {
        // WCVTF: write a FUnit value, scaled to current ppem.
        let idx = self.pop()? as usize;
        let value = self.pop()?;
        if idx < self.cvt.len() {
            let scaled = scale(value, self.scale_y);
            self.cvt[idx] = scaled;
            self.cvt_f[idx] = scaled as f64 / 64.0;
        }
        Ok(())
    }

    fn op_rcvt(&mut self) -> Result<()> {
        let idx = self.pop()? as usize;
        let v = self.cvt.get(idx).copied().unwrap_or(0);
        self.push(v)
    }

    /// GC 0x46 (original) / 0x47 (current): measure point `L` in zone zp2
    /// along the projection (0x46 dual) vector. Pops a point index and pushes
    /// the measured F26Dot6 coordinate — it does NOT touch the CVT table.
    fn op_gc(&mut self, opcode: u8) -> Result<()> {
        let idx = self.pop()? as usize;
        let encoded = (self.zp[2] << 15) | idx as u32;
        if self.point(encoded).is_err() {
            self.push(0)?; // out-of-range reads as zero like FreeType
            return Ok(());
        }
        let p = self.point(encoded)?;
        let value = if opcode & 1 != 0 {
            self.dual.dot(p.x, p.y)
        } else {
            self.projection.dot(p.x, p.y)
        };
        self.push(value)
    }

    fn op_scfs(&mut self) -> Result<()> {
        let idx = self.pop()? as usize;
        let value = self.pop()?;
        if idx < self.cvt.len() {
            self.cvt[idx] = value;
            self.cvt_f[idx] = value as f64 / 64.0;
        }
        Ok(())
    }

    fn op_md(&mut self, _opcode: u8) -> Result<()> {
        // MD 0x49 (grid) / 0x4A (off-grid): measure distance in pixels.
        let dist = self.pop()?;
        self.push(dist.abs())
    }

    // ---- rounding family ---------------------------------------------------

    fn op_round_family(&mut self, opcode: u8, no_round: bool) -> Result<()> {
        let v = self.pop()?;
        // 0x68/0x6C: gray; 0x69/0x6D: use current state; 0x6A/0x6E: black;
        // 0x6B/0x6F: white. Black/white bias the threshold; we approximate via
        // the current round state which fonts configure through SROUND anyway.
        let _ = opcode;
        if no_round {
            self.push(v)
        } else {
            self.push(self.round(v))
        }
    }

    fn op_s45round(&mut self) -> Result<()> {
        // S45ROUND uses the same packed parameter encoding as SROUND; the 45°
        // distinction matters only for non-square pixels, which we don't host.
        let v = self.pop()?;
        self.round_state = RoundState::from_sround(v);
        Ok(())
    }

    fn op_jrot(&mut self) -> Result<()> {
        let offset = self.pop()?;
        let value = self.pop()?;
        if self.round(value) != value {
            self.ip = (self.ip as isize + offset as isize) as usize;
        }
        Ok(())
    }

    fn op_jrof(&mut self) -> Result<()> {
        let offset = self.pop()?;
        let value = self.pop()?;
        if self.round(value) == value {
            self.ip = (self.ip as isize + offset as isize) as usize;
        }
        Ok(())
    }

    fn op_div(&mut self) -> Result<()> {
        // TrueType DIV: F26Dot6 / F26Dot6 -> F26Dot6, with the spec's
        // truncation-toward-zero and 0/0 = 0 rule.
        let b = self.pop()?;
        let a = self.pop()?;
        let q = if b == 0 {
            0
        } else {
            (((a as i64) << 6) / b as i64) as i32
        };
        self.push(q)
    }

    // ---- delta exceptions --------------------------------------------------

    fn op_deltap(&mut self, opcode: u8) -> Result<()> {
        // Stack (top first): nump, then nump (point A, packed B) pairs.
        let nump = self.pop()?.max(0) as usize;
        let ppem = self.ppem_y as i32;
        let mut p = ppem - self.delta_base;
        match opcode {
            0x71 => p -= 16,
            0x72 => p -= 32,
            _ => {}
        }
        if !(0..=15).contains(&p) {
            for _ in 0..nump {
                let _b = self.pop()?;
                let _a = self.pop()?;
            }
            return Ok(());
        }
        let p_nibble = p << 4;
        let f = 1i32 << (6 - self.delta_shift.clamp(0, 6));
        for _ in 0..nump {
            let b = self.pop()?;
            let point = self.pop()? as usize;
            if (b & 0xF0) != p_nibble {
                continue;
            }
            let mut mag = (b & 0x0F) - 8;
            if mag >= 0 {
                mag += 1;
            }
            let delta = mag * f;
            let encoded = (self.zp[0] << 15) | point as u32;
            if self.point(encoded).is_err() {
                continue; // ignore out-of-range delta points like FreeType
            }
            self.move_by_freedom(encoded, delta)?;
            self.touch(encoded)?;
        }
        Ok(())
    }

    fn op_deltac(&mut self, opcode: u8) -> Result<()> {
        // Stack (top first): nump, then nump pairs pushed as
        // (cvtIndex A, packed B) with B deepest. Mirrors Ins_DELTAC.
        let nump = self.pop()?.max(0) as usize;
        let ppem = self.ppem_y as i32;
        // P selects the ppem nibble: (ppem - delta_base) shifted by opcode.
        let mut p = ppem - self.delta_base;
        match opcode {
            0x74 => p -= 16,
            0x75 => p -= 32,
            _ => {}
        }
        // Only ppem values 0..=15 after the base shift can match a nibble.
        if !(0..=15).contains(&p) {
            // Still must consume the pair arguments.
            for _ in 0..nump {
                let _b = self.pop()?;
                let _a = self.pop()?;
            }
            return Ok(());
        }
        let p_nibble = p << 4;
        let f = 1i32 << (6 - self.delta_shift.clamp(0, 6));
        for _ in 0..nump {
            let b = self.pop()?;
            let a = self.pop()? as usize; // cvt index
            if (b & 0xF0) == p_nibble && a < self.cvt.len() {
                let mut mag = (b & 0x0F) - 8;
                if mag >= 0 {
                    mag += 1;
                }
                let delta = mag * f;
                self.cvt[a] += delta;
                self.cvt_f[a] = self.cvt[a] as f64 / 64.0;
            }
        }
        Ok(())
    }

    // ---- environment / scan conversion ------------------------------------

    fn op_getinfo(&mut self) -> Result<()> {
        let selector = self.pop()? as u32;
        // Bit 0: version (returns 0x00020000 style), bit 1: glyph rotation 0,
        // bit 2: grid stretched? no, bit 7: font scalar present -> true.
        let mut result = 0u32;
        if selector & 0x01 != 0 {
            result |= 0x00020000; // interpreter version 2.0
        }
        if selector & 0x40 != 0 {
            result |= 0x40;
        }
        if selector & 0x80 != 0 {
            result |= 0x80;
        }
        self.push(result as i32)
    }

    fn op_instctrl(&mut self) -> Result<()> {
        let _value = self.pop()?;
        let selector = self.pop()? as u32;
        // Selector 2 (native ClearType) and 3 (sideways) are unsupported;
        // record selector 1 (hinting enabled) only.
        if selector == 1 {
            self.instruct_control = 1;
        }
        Ok(())
    }

    fn op_sdpvtl(&mut self, opcode: u8) -> Result<()> {
        // SDPVTL: dual projection vector from two reference points.
        let p2_enc = self.encoded(2);
        let p1_enc = self.encoded(1);
        let (dx, dy) = {
            let p1 = self.point(p1_enc)?;
            let p2 = self.point(p2_enc)?;
            (p2.x - p1.x, p2.y - p1.y)
        };
        let mut v = normalize_f2dot14(dx, dy);
        if opcode == 0x87 {
            v = F2Dot14Vec {
                x: (-v.y) as i16,
                y: v.x,
            };
        }
        self.dual = v;
        self.projection = v;
        Ok(())
    }

    fn op_scvtci(&mut self) -> Result<()> {
        // SCVTCI: set CVT cut-in (F26Dot6); stored on min_distance slot since
        // our MIRP path consults min_distance uniformly.
        self.min_distance = self.pop()?;
        Ok(())
    }

    fn op_sswci(&mut self) -> Result<()> {
        // SSWCI: set single-width cut-in in FUnits; scale to pixels.
        let v = self.pop()?;
        self.min_distance = scale(v, self.scale_y);
        Ok(())
    }

    fn op_ssw(&mut self) -> Result<()> {
        // SSW: set single-width value; folded into min-distance handling.
        self.min_distance = self.pop()?;
        Ok(())
    }

    // ---- final IUP interpolation ------------------------------------------

    /// Interpolate untouched points between touched ones independently on each
    /// axis (the IUP pass FreeType runs after every glyph program).
    fn iup_interpolate(&mut self) {
        if self.glyph_pts.is_empty() {
            return;
        }
        let contour_ends = self.contour_ends_snapshot();
        for axis in [Axis::X, Axis::Y] {
            let mut start = 0usize;
            for &end in &contour_ends {
                self.iup_contour(axis, start, end as usize);
                start = end as usize + 1;
            }
        }
    }

    fn contour_ends_snapshot(&self) -> Vec<u16> {
        self.contour_ends.clone()
    }

    fn iup_contour(&mut self, axis: Axis, start: usize, end: usize) {
        if end < start {
            return;
        }
        let touch_flag = match axis {
            Axis::X => FLAG_TOUCH_X,
            Axis::Y => FLAG_TOUCH_Y,
        };
        let get = |p: &HintPoint| match axis {
            Axis::X => p.x,
            Axis::Y => p.y,
        };
        let set = |p: &mut HintPoint, v: i32| match axis {
            Axis::X => p.x = v,
            Axis::Y => p.y = v,
        };

        // First touched point anchors the contour; none touched -> leave as is.
        let first_touch = (start..=end).find(|&i| self.glyph_pts[i].flags & touch_flag != 0);
        let anchor = match first_touch {
            Some(i) => i,
            None => return,
        };

        // Untouched run before the anchor wraps to the end of the contour.
        let anchor_val = get(&self.glyph_pts[anchor]);
        for i in start..anchor {
            if self.glyph_pts[i].flags & touch_flag == 0 {
                set(&mut self.glyph_pts[i], anchor_val);
            }
        }

        let mut ref_idx = anchor;
        let mut i = anchor + 1;
        loop {
            if i > end {
                // Trailing untouched run inherits the last touched value.
                let v = get(&self.glyph_pts[ref_idx]);
                for j in ref_idx + 1..=end {
                    if self.glyph_pts[j].flags & touch_flag == 0 {
                        set(&mut self.glyph_pts[j], v);
                    }
                }
                break;
            }
            if self.glyph_pts[i].flags & touch_flag != 0 {
                // Interpolate untouched points between ref_idx and i using
                // original coordinates as the interpolation parameter.
                if i > ref_idx + 1 {
                    let o1 = get(&self.original[ref_idx]) as f64;
                    let o2 = get(&self.original[i]) as f64;
                    let v1 = get(&self.glyph_pts[ref_idx]) as f64;
                    let v2 = get(&self.glyph_pts[i]) as f64;
                    for j in ref_idx + 1..i {
                        if self.glyph_pts[j].flags & touch_flag == 0 {
                            let oj = get(&self.original[j]) as f64;
                            let t = if o2 == o1 {
                                0.0
                            } else {
                                (oj - o1) / (o2 - o1)
                            };
                            set(&mut self.glyph_pts[j], (v1 + (v2 - v1) * t) as i32);
                        }
                    }
                }
                ref_idx = i;
            }
            i += 1;
        }
    }
}

#[derive(Clone, Copy)]
enum Axis {
    X,
    Y,
}

/// Total byte length of the instruction starting at `p`, including any inline
/// push operands. Mirrors FreeType's `opcode_length` table plus the push ranges:
/// PUSHB[0..7] (0xB0..0xB7) carry 1..8 data bytes, PUSHW[0..7] (0xB8..0xBF)
/// carry 1..8 16-bit words.
fn instruction_len(bytes: &[u8], p: usize) -> usize {
    let op = bytes[p];
    match op {
        0x40 => {
            // NPUSHB: count byte + that many data bytes
            let n = *bytes.get(p + 1).unwrap_or(&0) as usize;
            2 + n
        }
        0x41 => {
            // NPUSHW: count byte + that many 2-byte values
            let n = *bytes.get(p + 1).unwrap_or(&0) as usize;
            2 + n * 2
        }
        0xB0..=0xB7 => 1 + (op - 0xB0 + 1) as usize,
        0xB8..=0xBF => 1 + (op - 0xB8 + 1) as usize * 2,
        _ => 1,
    }
}

///
/// Pre-scan a bytecode stream and build absolute jump targets for the
/// structured control-flow opcodes. Push operands are skipped via
/// [`instruction_len`] so data bytes can never be mistaken for IF/EIF/ELSE.
///
/// Indexing by the IP *at* the opcode:
/// - IF (0x58) -> IP just past the matching ELSE or EIF
/// - ELSE (0x1B) -> IP just past the matching EIF
/// - EIF (0x59) -> IP of its IF (unused at runtime but handy for diagnostics)
fn scan_jump_targets(bytes: &[u8]) -> Vec<Option<usize>> {
    let mut jump = vec![None; bytes.len()];
    // Each open IF records (if_ip, optional else_ip).
    let mut stack: Vec<(usize, Option<usize>)> = Vec::new();
    let mut p = 0usize;
    while p < bytes.len() {
        let op = bytes[p];
        match op {
            0x58 => {
                stack.push((p, None));
                p += 1;
            }
            0x1B => {
                if let Some(top) = stack.last_mut() {
                    top.1 = Some(p);
                }
                p += 1;
            }
            0x59 => {
                if let Some((if_ip, else_ip)) = stack.pop() {
                    let after = p + 1;
                    // IF false-branch target: ELSE's next instruction, else EIF.
                    jump[if_ip] = Some(else_ip.map(|e| e + 1).unwrap_or(after));
                    if let Some(e) = else_ip {
                        jump[e] = Some(after);
                    }
                    jump[p] = Some(if_ip);
                }
                p += 1;
            }
            _ => p += instruction_len(bytes, p),
        }
    }
    jump
}

/// Normalize an F26Dot6 (or integer) vector to an F2Dot14 unit vector.
fn normalize_f2dot14(x: i32, y: i32) -> F2Dot14Vec {
    if x == 0 && y == 0 {
        return F2Dot14Vec::Y;
    }
    let len = ((x as f64).powi(2) + (y as f64).powi(2)).sqrt();
    let nx = (x as f64 / len * 16384.0).round() as i16;
    let ny = (y as f64 / len * 16384.0).round() as i16;
    F2Dot14Vec { x: nx, y: ny }
}

// Keep mul_fixed reachable for table-level scaling helpers.
#[allow(dead_code)]
fn _use_mul_fixed(a: i32, b: i32) -> i32 {
    mul_fixed(a, b)
}
