//! Error codes returned across the C ABI. Numeric values are part of the contract.

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Ok = 0,
    /// Not a recognizable SFNT/TrueType font.
    NotTrueType = 1,
    /// A required table is missing.
    MissingTable = 2,
    /// Offset/length outside the font buffer, or truncated table.
    BadOffset = 3,
    /// Unsupported table format or flag combination.
    Unsupported = 4,
    /// Composite glyph recursion exceeded the depth limit.
    CompositeDepth = 5,
    /// Glyph index is out of range.
    BadGlyphIndex = 6,
    /// TrueType bytecode exceeded an execution budget (loop/jump/call limits).
    BytecodeBudget = 7,
    /// Bytecode referenced an invalid point/cvt/function index.
    BytecodeIndex = 8,
    /// Output bitmap buffer is too small for the request.
    BufferTooSmall = 9,
    /// Invalid API argument (null pointer, zero size, ...).
    InvalidArg = 10,
    /// Out of memory while growing an internal vector.
    Alloc = 11,
    /// No outline for this glyph (space, .notdef-empty, ...).
    EmptyGlyph = 12,
    /// Unsupported sfnt version (e.g. CFF/OpenType outlines).
    CffUnsupported = 13,
}

impl Error {
    #[inline]
    pub fn into_i32(self) -> i32 {
        self as i32
    }
}

pub type Result<T> = core::result::Result<T, Error>;
