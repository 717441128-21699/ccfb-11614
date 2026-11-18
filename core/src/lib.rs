//! ttengine —— 自研 TrueType 解析 / 微调 / 栅格化核心(零依赖)。
//!
//! 模块划分:
//! - [`fixed`]:   26.6 / 2.14 定点数与舍入
//! - [`font`]:    字体文件、表目录、TTC 集合
//! - [`tables`]:  head/maxp/hhea/hmtx/cmap/loca 等表解析
//! - [`glyf`]:    字形轮廓(简单字形 + 复合字形)
//! - [`hint`]:    TrueType 微调指令解释器
//! - [`raster`]:  扫描线解析覆盖率栅格化
//! - [`atlas`]:   按需栅格化 + 纹理图集缓存
//! - [`engine`]:  高层门面,串起完整管线
//! - [`ffi`]:     C ABI 导出(供 WASM 宿主调用)

pub mod atlas;
pub mod engine;
pub mod ffi;
pub mod fixed;
pub mod font;
pub mod glyf;
pub mod hint;
pub mod raster;
pub mod tables;

/// 全引擎统一错误类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// 数据不足或偏移越界
    OutOfBounds { what: &'static str, offset: usize },
    /// 魔数 / 标签不符合 TrueType 规范
    BadMagic { what: &'static str, found: u32 },
    /// 遇到暂不支持的表格式或特性
    Unsupported { what: &'static str },
    /// 字形索引越界等
    BadGlyph(u16),
    /// 图集已满且无法继续增长
    AtlasFull,
}

pub type Result<T> = core::result::Result<T, Error>;

impl Error {
    pub fn oob(what: &'static str, offset: usize) -> Self {
        Error::OutOfBounds { what, offset }
    }
}
