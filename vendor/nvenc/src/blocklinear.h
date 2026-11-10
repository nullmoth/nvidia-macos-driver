// blocklinear.h: NVIDIA block-linear surface layout (Fermi and later, "GPU BL"): 64-byte x 8-row GOBs of 512 bytes;
// a block is one GOB wide and 2^block_height GOBs tall; blocks are stored row by row.
#pragma once
#include <stddef.h>
#include <stdint.h>

// byte offset of (x bytes, y rows) in a surface whose rows are pitch bytes (multiple of 64)
size_t bl_offset(size_t x, size_t y, size_t pitch, unsigned block_height_log2);
// size of a plane (rows rounded up to the block height)
size_t bl_plane_size(size_t pitch, size_t rows, unsigned block_height_log2);
// copy a pitch-linear plane (src_stride) of width x rows bytes into block-linear dst
void bl_tile_plane(uint8_t *dst, const uint8_t *src, size_t src_stride, size_t width, size_t rows, size_t pitch, unsigned bh);

// The layout NVENC on Ada reads for input_cfg block_height 2 / input_bl_mode 0, measured from a working client's input
// surface (T2.05d): 64-byte x 16-row tiles of 1 KB, row-major; inside a tile, 16-byte x 4-row sub-blocks ordered
// x16, y4, x32, y8. Planes are rounded up to 16 rows.
size_t nvenc_in_offset(size_t x, size_t y, size_t pitch);
size_t nvenc_in_plane_size(size_t pitch, size_t rows);
void nvenc_in_tile_plane(uint8_t *dst, const uint8_t *src, size_t src_stride, size_t width, size_t rows, size_t pitch);
// the inverse: a plane in that layout (pitch) to pitch-linear dst (dst_stride); NVDEC writes the same layout (M8)
void nvenc_in_untile_plane(uint8_t *dst, size_t dst_stride, const uint8_t *src, size_t width, size_t rows, size_t pitch);
// The layout of a decoded plane. A plane is rows of 64-byte x 16-row tiles of 1 KB (two 512-byte GOBs, one above the
// other); the generations differ in how the bytes lie inside a GOB. Mesa's NIL (src/nouveau/nil/tiling.rs and copy.rs,
// MIT) gives the rule and the three layouts: before Blackwell every colour plane is "TuringColor2D" (nvenc_in_offset
// above); on Blackwell a plane of 1-byte elements (8-bit luma) is "Blackwell8Bit", one of 2-byte elements (8-bit
// interleaved chroma, 16-bit luma) is "Blackwell16Bit", and wider elements keep the Turing layout.
enum { GOB_TURING = 0, GOB_BLACKWELL8 = 1, GOB_BLACKWELL16 = 2 };
// The layout of a plane whose elements are elemBytes wide (1, 2 or 4) on a GPU whose NVDEC class is nvdecClass
// (0xcfb0 is Blackwell's). NVDEC_GOB=turing or NVDEC_GOB=blackwell in the environment overrides the class.
int gob_layout_for(unsigned nvdecClass, unsigned elemBytes);
// byte offset of (x bytes, y rows) in a plane of that layout
size_t gob_offset(int layout, size_t x, size_t y, size_t pitch);
// a plane in that layout (pitch) to pitch-linear dst (dst_stride); width in bytes
void gob_untile_plane(int layout, uint8_t *dst, size_t dst_stride, const uint8_t *src, size_t width, size_t rows, size_t pitch);
// BGRA (8-bit, 4 bytes per pixel) -> NV12 in NVENC's input layout, BT.709 video range, 2x2 chroma average; converted per
// 16-row band in a cache-sized buffer, bands over two threads. dst_luma / dst_chroma: the two planes (pitch bytes per row).
void nvenc_in_tile_bgra(uint8_t *dst_luma, uint8_t *dst_chroma, const uint8_t *src, size_t src_stride, size_t width, size_t rows, size_t pitch);
// l10r (ARGB2101010 little-endian packed: B bits 0-9, G 10-19, R 20-29; ScreenCaptureKit's 10-bit format) -> 10-bit 4:2:0
// (P010 samples: value << 6) in NVENC's input layout, BT.709 video range, 2x2 chroma average. pitch: bytes per row
// (2 x the pitch in samples). Converted per 32-row super-band into a cache-sized buffer, bands over threads.
void nvenc_in_tile_l10r(uint8_t *dst_luma, uint8_t *dst_chroma, const uint8_t *src, size_t src_stride, size_t width, size_t rows, size_t pitch);
