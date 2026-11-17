//! WebAssembly core entry points.
//!
//! The ABI is a plain C-style surface so the TypeScript side can use
//! `WebAssembly.instantiate` directly with no bindings generator:
//!
//! - font bytes are copied into linear memory once (`tt_font_open`)
//! - glyph results are written into caller-provided buffers
//! - every call returns 0 on success or an [`Error`] code
//!
//! Memory management is explicit: `tt_alloc`/`tt_free` hand the host stable
//! regions; font handles and output buffers are released by matching frees.

// The crate exposes a broad C ABI plus helper routines that not every build
// path calls directly; dead-code and unused-import lints on that surface are
// intentional rather than defects.
#![allow(dead_code, unused_imports, unused_variables)]

mod cmap;
mod error;
mod fixed;
mod font;
mod glyph;
mod interp;
mod raster;
mod reader;
mod tables;

use error::Error;
use font::{FontEngine, GlyphMetrics};

use core::cell::UnsafeCell;

struct ErrorBuffer(UnsafeCell<[u8; 96]>);
unsafe impl Sync for ErrorBuffer {}

static LAST_ERROR: ErrorBuffer = ErrorBuffer(UnsafeCell::new([0; 96]));


fn set_error(msg: &str) {
    unsafe {
        let buf = LAST_ERROR.0.get() as *mut u8;
        let n = msg.len().min(95);
        core::ptr::copy_nonoverlapping(msg.as_ptr(), buf, n);
        *buf.add(n) = 0;
    }
}

#[no_mangle]
pub extern "C" fn tt_last_error_message(out: *mut u8, cap: usize) -> usize {
    unsafe {
        let base = LAST_ERROR.0.get() as *const u8;
        let mut len = 0usize;
        while len < 96 && *base.add(len) != 0 {
            len += 1;
        }
        let n = len.min(cap.saturating_sub(1));
        if !out.is_null() && cap > 0 {
            core::ptr::copy_nonoverlapping(base, out, n);
            *out.add(n) = 0;
        }
        n
    }
}

/// Allocate `size` bytes in linear memory. Returns 0 on failure.
#[no_mangle]
pub extern "C" fn tt_alloc(size: usize) -> *mut u8 {
    let mut buf: Vec<u8> = Vec::with_capacity(size.max(1));
    let ptr = buf.as_mut_ptr();
    core::mem::forget(buf);
    ptr
}

#[no_mangle]
pub extern "C" fn tt_free(ptr: *mut u8, size: usize, capacity: usize) {
    if ptr.is_null() || capacity == 0 {
        return;
    }
    unsafe {
        drop(Vec::from_raw_parts(ptr, size, capacity));
    }
}

/// Box a font engine and return an opaque handle. `font_index` selects a face
/// inside a TTC collection (0 for plain TTF).
#[no_mangle]
pub extern "C" fn tt_font_open(
    data_ptr: *const u8,
    data_len: usize,
    font_index: u32,
) -> u64 {
    if data_ptr.is_null() || data_len == 0 {
        set_error("null font data");
        return 0;
    }
    let bytes = unsafe { core::slice::from_raw_parts(data_ptr, data_len) }.to_vec();
    match FontEngine::open(bytes, font_index) {
        Ok(engine) => Box::into_raw(Box::new(engine)) as u64,
        Err(e) => {
            set_error(error_name(e));
            0
        }
    }
}

#[no_mangle]
pub extern "C" fn tt_font_close(handle: u64) {
    if handle == 0 {
        return;
    }
    unsafe {
        drop(Box::from_raw(handle as *mut FontEngine));
    }
}

#[no_mangle]
pub extern "C" fn tt_num_glyphs(handle: u64) -> u32 {
    with_engine(handle, |f| f.num_glyphs() as u32).unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn tt_units_per_em(handle: u64) -> u32 {
    with_engine(handle, |f| f.units_per_em() as u32).unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn tt_codepoint_to_gid(handle: u64, codepoint: u32) -> u32 {
    with_engine(handle, |f| f.codepoint_to_gid(codepoint) as u32).unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn tt_set_pixel_size(handle: u64, ppem: u16, hinting: u32) -> i32 {
    let r = with_engine_mut(handle, |f| {
        f.set_hinting(hinting != 0);
        f.set_pixel_size(ppem)
    });
    match r {
        Some(Ok(())) => Error::Ok.into_i32(),
        Some(Err(e)) => {
            set_error(error_name(e));
            e.into_i32()
        }
        None => Error::InvalidArg.into_i32(),
    }
}

/// Output block matching `GlyphMetrics` plus the alpha plane location.
#[repr(C)]
pub struct GlyphResult {
    pub width: u32,
    pub height: u32,
    pub left: i32,
    pub top: i32,
    pub advance: f32,
    pub lsb: f32,
    pub alpha_ptr: u32, // offset into linear memory, 0 when empty
    pub alpha_len: u32,
}

#[no_mangle]
pub extern "C" fn tt_render_glyph(handle: u64, gid: u32, out: *mut GlyphResult) -> i32 {
    if out.is_null() {
        set_error("null GlyphResult");
        return Error::InvalidArg.into_i32();
    }
    let result = with_engine_mut(handle, |f| f.render_glyph(gid as u16));
    match result {
        Some(Ok((alpha, m))) => {
            let (alpha_ptr, alpha_len) = if alpha.is_empty() {
                (0u32, 0u32)
            } else {
                let len = alpha.len() as u32;
                let boxed = alpha.into_boxed_slice();
                let ptr = boxed.as_ptr() as u32;
                core::mem::forget(boxed);
                (ptr, len)
            };
            unsafe {
                *out = GlyphResult {
                    width: m.width,
                    height: m.height,
                    left: m.left,
                    top: m.top,
                    advance: m.advance,
                    lsb: m.lsb,
                    alpha_ptr,
                    alpha_len,
                };
            }
            Error::Ok.into_i32()
        }
        Some(Err(e)) => {
            set_error(error_name(e));
            e.into_i32()
        }
        None => Error::InvalidArg.into_i32(),
    }
}

/// Free an alpha plane previously returned through [`tt_render_glyph`].
#[no_mangle]
pub extern "C" fn tt_alpha_free(ptr: u32, len: u32) {
    if ptr == 0 || len == 0 {
        return;
    }
    unsafe {
        let slice_ptr = core::ptr::slice_from_raw_parts_mut(ptr as *mut u8, len as usize);
        drop(Box::from_raw(slice_ptr));
    }
}

#[repr(C)]
pub struct LineMetrics {
    pub ascender: f32,
    pub descender: f32,
    pub line_gap: f32,
}

/// Diagnostic block for the most recent interpreter fault.
#[repr(C)]
pub struct FaultBlock {
    pub opcode: u32,
    pub ip: u32,
    pub code_len: u32,
    pub stack_depth: u32,
    pub budget: u64,
    pub in_global: u32,
    pub stage: u32,
}

#[no_mangle]
pub extern "C" fn tt_last_fault(handle: u64, out: *mut FaultBlock) -> i32 {
    let Some(engine) = (if handle == 0 {
        None
    } else {
        unsafe { (handle as *const FontEngine).as_ref() }
    }) else {
        return Error::InvalidArg.into_i32();
    };
    let Some(f) = engine.fault_info() else {
        return 1; // no fault recorded
    };
    if out.is_null() {
        return Error::InvalidArg.into_i32();
    }
    unsafe {
        *out = FaultBlock {
            opcode: f.opcode as u32,
            ip: f.ip as u32,
            code_len: f.code_len as u32,
            stack_depth: f.stack_depth as u32,
            budget: f.budget,
            in_global: f.in_global as u32,
            stage: f.stage as u32,
        };
    }
    Error::Ok.into_i32()
}

/// Copy the recent-opcode trace (up to 64 bytes) into `out`. Returns length.
#[no_mangle]
pub extern "C" fn tt_fault_trace(handle: u64, out: *mut u8, cap: usize) -> usize {
    let Some(engine) = (if handle == 0 {
        None
    } else {
        unsafe { (handle as *const FontEngine).as_ref() }
    }) else {
        return 0;
    };
    let Some(interp) = engine.interpreter_ref() else {
        return 0;
    };
    let trace = &interp.trace;
    let n = trace.len().min(cap);
    if out.is_null() || n == 0 {
        return trace.len();
    }
    unsafe {
        core::ptr::copy_nonoverlapping(trace.as_ptr(), out, n);
    }
    n
}

/// Copy the stack-depth trace as u32 little-endian values. Returns count.
#[no_mangle]
pub extern "C" fn tt_fault_trace_depths(handle: u64, out: *mut u32, cap: usize) -> usize {
    let Some(engine) = (if handle == 0 {
        None
    } else {
        unsafe { (handle as *const FontEngine).as_ref() }
    }) else {
        return 0;
    };
    let depths = engine.trace_depths();
    let n = depths.len().min(cap);
    if out.is_null() || n == 0 {
        return depths.len();
    }
    unsafe {
        for i in 0..n {
            *out.add(i) = depths[i];
        }
    }
    n
}

#[no_mangle]
pub extern "C" fn tt_line_metrics(handle: u64, out: *mut LineMetrics) -> i32 {
    if out.is_null() {
        return Error::InvalidArg.into_i32();
    }
    match with_engine(handle, |f| f.line_metrics()) {
        Some((ascender, descender, line_gap)) => {
            unsafe {
                *out = LineMetrics {
                    ascender,
                    descender,
                    line_gap,
                };
            }
            Error::Ok.into_i32()
        }
        None => Error::InvalidArg.into_i32(),
    }
}

#[inline]
fn with_engine<R>(handle: u64, f: impl FnOnce(&FontEngine) -> R) -> Option<R> {
    if handle == 0 {
        return None;
    }
    let engine = unsafe { (handle as *const FontEngine).as_ref()? };
    Some(f(engine))
}

#[inline]
fn with_engine_mut<R>(handle: u64, f: impl FnOnce(&mut FontEngine) -> R) -> Option<R> {
    if handle == 0 {
        return None;
    }
    let engine = unsafe { (handle as *mut FontEngine).as_mut()? };
    Some(f(engine))
}

fn error_name(e: Error) -> &'static str {
    match e {
        Error::Ok => "ok",
        Error::NotTrueType => "not a TrueType font",
        Error::MissingTable => "missing required table",
        Error::BadOffset => "truncated or out-of-range table",
        Error::Unsupported => "unsupported font feature",
        Error::CompositeDepth => "composite glyph too deep",
        Error::BadGlyphIndex => "glyph index out of range",
        Error::BytecodeBudget => "bytecode execution budget exceeded",
        Error::BytecodeIndex => "bytecode referenced invalid index",
        Error::BufferTooSmall => "output buffer too small",
        Error::InvalidArg => "invalid API argument",
        Error::Alloc => "allocation failed",
        Error::EmptyGlyph => "empty glyph",
        Error::CffUnsupported => "CFF/OpenType outlines are not supported",
    }
}
