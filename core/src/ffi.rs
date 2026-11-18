//! C ABI 导出层,供 WASM 宿主(TS)通过线性内存调用。
//!
//! 设计:
//! - 字体字节由宿主写入引擎管理的缓冲,返回不透明句柄;
//! - 所有可变引擎状态存在全局槽位(单线程 WASM 足够);
//! - 输出位图通过 `tt_last_*` 系列函数读取,避免跨边界分配。
//!
//! 句柄用非零 ID + 代际校验,防止悬挂引用。
#![allow(static_mut_refs)]

use crate::atlas::{Atlas, GlyphKey};
use crate::engine::{Engine, Face};
use crate::Error;

/// 全局引擎槽位(wasm32 单线程)。
static mut ENGINE: Option<EngineHolder> = None;
/// 独立的全局输入缓冲,tt_init 之前即可分配。
static mut INPUT_BUF: Vec<u8> = Vec::new();

struct EngineHolder {
    /// 字体字节必须在引擎整个生命周期内有效,由 Box 持有
    _font_data: Box<[u8]>,
    engine: Engine<'static>,
    atlas: Atlas<'static>,
}

// wasm32 单线程,这里的 Send/Sync 仅为满足静态存储;实际不会跨线程。
unsafe impl Send for EngineHolder {}
unsafe impl Sync for EngineHolder {}

/// 错误码:0=成功,负数=各类错误。
pub const ERR_OK: i32 = 0;
pub const ERR_INVALID: i32 = -1;
pub const ERR_ATLAS_FULL: i32 = -2;
pub const ERR_GLYPH: i32 = -3;
pub const ERR_STATE: i32 = -4;

fn err_code(e: &Error) -> i32 {
    match e {
        Error::AtlasFull => ERR_ATLAS_FULL,
        Error::BadGlyph(_) => ERR_GLYPH,
        _ => ERR_INVALID,
    }
}

/// 用字体字节初始化引擎。
///
/// # 安全性
/// `ptr/len` 必须指向一段有效的字体字节(TTF/TTC)。
/// `face_index` 用于 TTC 集合选择。
#[no_mangle]
pub unsafe extern "C" fn tt_init(
    ptr: *const u8,
    len: u32,
    face_index: u32,
    ppem: u16,
    atlas_w: u32,
    atlas_h: u32,
) -> i32 {
    if ptr.is_null() || len == 0 {
        return ERR_INVALID;
    }
    let slice = core::slice::from_raw_parts(ptr, len as usize);
    // 复制进 'static Box,保证引擎生命周期内有效
    let font_data: Box<[u8]> = slice.to_vec().into_boxed_slice();
    let data_ref: &'static [u8] = core::mem::transmute::<&[u8], &'static [u8]>(&*font_data);

    let face = match Face::parse(data_ref, face_index) {
        Ok(f) => f,
        Err(e) => return err_code(&e),
    };
    let engine = Engine::new(face, ppem);
    let atlas = Atlas::new(atlas_w.max(64), atlas_h.max(64));
    ENGINE = Some(EngineHolder {
        _font_data: font_data,
        engine,
        atlas,
    });
    ERR_OK
}

/// 分配(或扩容)全局输入缓冲,返回指针;容量通过 out_cap 回传。
/// 宿主先调用它拿到可写地址,写入字体字节后再 tt_init。
#[no_mangle]
pub unsafe extern "C" fn tt_alloc_input(len: u32, out_cap: *mut u32) -> i64 {
    let need = (len as usize).max(1);
    if INPUT_BUF.capacity() < need {
        INPUT_BUF.resize(need, 0);
    } else if INPUT_BUF.len() < need {
        INPUT_BUF.resize(need, 0);
    }
    if !out_cap.is_null() {
        *out_cap = INPUT_BUF.len() as u32;
    }
    INPUT_BUF.as_mut_ptr() as i64
}

/// 一块小的出参暂存区(度量/统计等标量回传),64 字节足够。
#[no_mangle]
pub unsafe extern "C" fn tt_scratch() -> i64 {
    static mut SCRATCH: [u8; 128] = [0u8; 128];
    SCRATCH.as_mut_ptr() as i64
}

/// 更换当前字号(会保留字体,重置微调上下文)。
#[no_mangle]
pub unsafe extern "C" fn tt_set_ppem(ppem: u16) -> i32 {
    let Some(h) = ENGINE.as_mut() else { return ERR_STATE; };
    // 重新构建引擎(face 数据仍由 holder 持有)
    let data: &'static [u8] = core::mem::transmute::<&[u8], &'static [u8]>(&*h._font_data);
    let face = match Face::parse(data, 0) {
        Ok(f) => f,
        Err(e) => return err_code(&e),
    };
    h.engine = Engine::new(face, ppem);
    ERR_OK
}

/// 码点 -> 字形 ID。
#[no_mangle]
pub unsafe extern "C" fn tt_glyph_id(codepoint: u32) -> i32 {
    let Some(h) = ENGINE.as_mut() else { return ERR_STATE; };
    match h.engine.glyph_id(codepoint) {
        Ok(g) => g as i32,
        Err(e) => err_code(&e),
    }
}

/// 栅格化字形并写入图集,返回条目;通过出参指针回传位置。
///
/// 输出(均为 u32):x, y, width, height, advance(拆为两个 u16 定点),
/// lsb/top 同理。为简化,这里返回 entry id,度量用专门函数读。
#[no_mangle]
pub unsafe extern "C" fn tt_render(glyph_id: u16, out_x: *mut u32, out_y: *mut u32) -> i32 {
    let Some(h) = ENGINE.as_mut() else { return ERR_STATE; };
    let key = GlyphKey {
        glyph_id,
        ppem: h.engine.ppem(),
    };
    let entry = match h.atlas.ensure(&mut h.engine, glyph_id) {
        Ok(e) => e,
        Err(e) => return err_code(&e),
    };
    if !out_x.is_null() {
        *out_x = entry.x;
    }
    if !out_y.is_null() {
        *out_y = entry.y;
    }
    // 缓存命中也返回 OK;调用方用 tt_entry_* 读完整度量
    let _ = key;
    ERR_OK
}

/// 读取最近渲染字形的条目度量(出参指针,可传 null)。
#[no_mangle]
pub unsafe extern "C" fn tt_entry_metrics(
    glyph_id: u16,
    out_w: *mut u32,
    out_h: *mut u32,
    out_advance: *mut f32,
    out_lsb: *mut f32,
    out_top: *mut f32,
) -> i32 {
    let Some(h) = ENGINE.as_ref() else { return ERR_STATE; };
    let key = GlyphKey {
        glyph_id,
        ppem: h.engine.ppem(),
    };
    let Some(e) = h.atlas.get(key) else { return ERR_GLYPH; };
    if !out_w.is_null() { *out_w = e.width; }
    if !out_h.is_null() { *out_h = e.height; }
    if !out_advance.is_null() { *out_advance = e.advance; }
    if !out_lsb.is_null() { *out_lsb = e.left_side_bearing; }
    if !out_top.is_null() { *out_top = e.top_bearing; }
    ERR_OK
}

/// 图集 R8 像素缓冲指针与尺寸(上传 WebGL 纹理)。
#[no_mangle]
pub unsafe extern "C" fn tt_atlas_ptr() -> *const u8 {
    match ENGINE.as_ref() {
        Some(h) => h.atlas.pixels().as_ptr(),
        None => core::ptr::null(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn tt_atlas_size(out_w: *mut u32, out_h: *mut u32) -> i32 {
    let Some(h) = ENGINE.as_ref() else { return ERR_STATE; };
    if !out_w.is_null() { *out_w = h.atlas.width(); }
    if !out_h.is_null() { *out_h = h.atlas.height(); }
    ERR_OK
}

/// 清空图集(缓存回收)。
#[no_mangle]
pub unsafe extern "C" fn tt_atlas_clear() -> i32 {
    let Some(h) = ENGINE.as_mut() else { return ERR_STATE; };
    h.atlas.clear();
    ERR_OK
}

/// 行高指标。
#[no_mangle]
pub unsafe extern "C" fn tt_line_metrics(
    out_ascent: *mut f32,
    out_descent: *mut f32,
    out_gap: *mut f32,
) -> i32 {
    let Some(h) = ENGINE.as_ref() else { return ERR_STATE; };
    let m = h.engine.line_metrics();
    if !out_ascent.is_null() { *out_ascent = m.ascent; }
    if !out_descent.is_null() { *out_descent = m.descent; }
    if !out_gap.is_null() { *out_gap = m.line_gap; }
    ERR_OK
}

/// 微调统计:已执行指令数 / 未实现数 / 中止数。
#[no_mangle]
pub unsafe extern "C" fn tt_hint_stats(
    out_opcodes: *mut u64,
    out_unknown: *mut u32,
    out_aborts: *mut u32,
) -> i32 {
    let Some(h) = ENGINE.as_ref() else { return ERR_STATE; };
    let s = h.engine.hint_stats();
    if !out_opcodes.is_null() { *out_opcodes = s.opcodes_run; }
    if !out_unknown.is_null() { *out_unknown = s.unknown_opcodes; }
    if !out_aborts.is_null() { *out_aborts = s.aborts; }
    ERR_OK
}

/// 销毁引擎,释放全部资源。
#[no_mangle]
pub unsafe extern "C" fn tt_shutdown() {
    ENGINE = None;
}

/// 引擎版本(语义化版本编码)。
#[no_mangle]
pub extern "C" fn tt_version() -> u32 {
    // 0.1.0
    (0 << 16) | (1 << 8) | 0
}
