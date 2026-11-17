# Reference materials — read-only, no code copied

The files in this directory were used **only as semantic references** while
writing the Rust interpreter and parser. No source code is linked or vendored
into the build; the engine in `../src` is an original implementation.

## FreeType (`ttinterp.c`, `ttinterp.h`, `ttobjs.c`)

Upstream: https://gitlab.freedesktop.org/freetype/freetype (src/truetype/)

These document the exact TrueType bytecode opcode semantics, the default
graphics state, and the rounding/delta conventions. FreeType is dual-licensed
under the FreeType License (BSD-style) or GPLv2; its copyright and license
terms apply to these reference files only.

## stb (`stb_truetype.h`)

Upstream: https://github.com/nothings/stb

Used to cross-check the SFNT table walk, the push-opcode length table, and
general rasterization approach. Dual-licensed public domain / MIT. See
`LICENSE-stb.txt`.
