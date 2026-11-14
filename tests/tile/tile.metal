#include <metal_stdlib>
using namespace metal;

// Shaders for the tile shading oracles. Pixel values are small integers so that a unorm8 attachment holds them
// exactly and the CPU can recompute every expectation.

struct Color0 { half4 c [[color(0)]]; };
struct RectU { float4 rect; float4 color; };
struct GB { half4 a [[raster_order_group(0)]]; half b [[raster_order_group(0)]]; };
struct GBOut { GB gb [[imageblock_data]]; };
// Explicit imageblock with every member type a storage-image plane carries: float4, int, uint, uchar4, uchar.
struct GB2 {
    float4 f [[raster_order_group(0)]];
    int i [[raster_order_group(0)]];
    uint u [[raster_order_group(0)]];
    uchar4 c [[raster_order_group(0)]];
    uchar b [[raster_order_group(0)]];
};
struct GB2Out { GB2 gb [[imageblock_data]]; };

//@ entry vs_full
vertex float4 vs_full(uint vid [[vertex_id]]) {
    float2 p = float2((vid << 1) & 2, vid & 2);
    return float4(p * 2.0 - 1.0, 0.0, 1.0);
}

//@ entry vs_rect
vertex float4 vs_rect(uint vid [[vertex_id]], constant RectU &u [[buffer(0)]]) {
    float2 c = float2(vid & 1, (vid >> 1) & 1);
    float2 p = float2(u.rect.x, u.rect.y) + c * float2(u.rect.z, u.rect.w);
    return float4(p, 0.0, 1.0);
}

//@ entry fs_const
fragment half4 fs_const(float4 pos [[position]], constant RectU &u [[buffer(0)]]) {
    return half4(u.color);
}

//@ entry fs_pattern
fragment half4 fs_pattern(float4 pos [[position]]) {
    uint x = uint(pos.x), y = uint(pos.y);
    return half4(float4(float((x * 4) & 255), float((y * 5) & 255), float(((x ^ y) & 1) * 200), 255.0) / 255.0);
}

// Arm A/B/C: implicit imageblock over colour attachment 0. Per-tile average into a buffer, then the average is
// written back into every pixel of the tile.
//@ entry tile_avg
kernel void tile_avg(imageblock<Color0, imageblock_layout_implicit> blk,
                     threadgroup float4 *sum [[threadgroup(0)]],
                     device float4 *outAvg [[buffer(0)]],
                     constant uint &tilesX [[buffer(1)]],
                     ushort2 lid [[thread_position_in_threadgroup]],
                     ushort2 tgsz [[threads_per_threadgroup]],
                     ushort2 tile [[threadgroup_position_in_grid]],
                     ushort tidx [[thread_index_in_threadgroup]]) {
    sum[tidx] = float4(blk.read(lid).c);
    threadgroup_barrier(mem_flags::mem_threadgroup);
    ushort n = tgsz.x * tgsz.y;
    for (ushort s = n / 2; s > 0; s >>= 1) {
        if (tidx < s) sum[tidx] += sum[tidx + s];
        threadgroup_barrier(mem_flags::mem_threadgroup);
    }
    float4 avg = sum[0] / float(n);
    if (tidx == 0) outAvg[uint(tile.y) * tilesX + tile.x] = avg;
    Color0 w; w.c = half4(avg);
    blk.write(w, lid);
}

// Arm F: two threadgroup memory arguments (atomic histogram + scratch) with different lengths.
//@ entry tile_hist
kernel void tile_hist(imageblock<Color0, imageblock_layout_implicit> blk,
                      threadgroup atomic_uint *hist [[threadgroup(0)]],
                      threadgroup float *scratch [[threadgroup(1)]],
                      device uint *outHist [[buffer(0)]],
                      constant uint &tilesX [[buffer(1)]],
                      ushort2 lid [[thread_position_in_threadgroup]],
                      ushort2 tile [[threadgroup_position_in_grid]],
                      ushort tidx [[thread_index_in_threadgroup]]) {
    if (tidx < 16) atomic_store_explicit(&hist[tidx], 0u, memory_order_relaxed);
    threadgroup_barrier(mem_flags::mem_threadgroup);
    scratch[tidx] = float(blk.read(lid).c.r);
    uint bin = min(uint(scratch[tidx] * 15.999), 15u);
    atomic_fetch_add_explicit(&hist[bin], 1u, memory_order_relaxed);
    threadgroup_barrier(mem_flags::mem_threadgroup);
    if (tidx < 16) outHist[(uint(tile.y) * tilesX + tile.x) * 16 + tidx] = atomic_load_explicit(&hist[tidx], memory_order_relaxed);
}


//@ entry fs_gb_init
fragment GBOut fs_gb_init(float4 pos [[position]]) {
    GBOut o;
    uint x = uint(pos.x), y = uint(pos.y);
    o.gb.a = half4(float4(float((x * 4) & 255), float((y * 5) & 255), 7.0, 255.0) / 255.0);
    o.gb.b = half(1.0);
    return o;
}

//@ entry fs_gb_acc
fragment GBOut fs_gb_acc(float4 pos [[position]], GB prev [[imageblock_data]]) {
    GBOut o;
    o.gb = prev;
    o.gb.b = prev.b + half(1.0);
    return o;
}

//@ entry tile_resolve
kernel void tile_resolve(imageblock<GB, imageblock_layout_explicit> blk,
                         texture2d<half, access::write> out [[texture(0)]],
                         ushort2 lid [[thread_position_in_threadgroup]],
                         ushort2 tgsz [[threads_per_threadgroup]],
                         ushort2 tile [[threadgroup_position_in_grid]]) {
    threadgroup_imageblock GB *p = blk.data(lid);
    uint2 xy = uint2(tile) * uint2(tgsz) + uint2(lid);
    out.write(half4(p->a.x, p->a.y, p->b / half(8.0), 1.0h), xy);
}

// Arm 5: the tile coordinate and the thread position inside the tile, written into the pixel.
//@ entry tile_ids
kernel void tile_ids(imageblock<Color0, imageblock_layout_implicit> blk,
                     ushort2 lid [[thread_position_in_threadgroup]],
                     ushort2 tile [[threadgroup_position_in_grid]]) {
    Color0 w;
    w.c = half4(half(tile.x), half(tile.y), half(lid.x), half(lid.y)) / half(255.0);
    blk.write(w, lid);
}

// Arm 8: reads the attachment as the pass loaded it (a Clear value) and adds one.
//@ entry tile_addone
kernel void tile_addone(imageblock<Color0, imageblock_layout_implicit> blk,
                        ushort2 lid [[thread_position_in_threadgroup]]) {
    Color0 v = blk.read(lid);
    v.c.r += half(1.0 / 255.0);
    blk.write(v, lid);
}

// Arm 9: every thread copies the attachment texel it sees into a buffer, indexed by pixel, to show exactly what a
// tile kernel reads after a draw.
//@ entry tile_dump
kernel void tile_dump(imageblock<Color0, imageblock_layout_implicit> blk,
                      device float4 *dump [[buffer(0)]],
                      constant uint &width [[buffer(1)]],
                      ushort2 lid [[thread_position_in_threadgroup]],
                      ushort2 tgsz [[threads_per_threadgroup]],
                      ushort2 tile [[threadgroup_position_in_grid]]) {
    uint2 xy = uint2(tile) * uint2(tgsz) + uint2(lid);
    dump[xy.y * width + xy.x] = float4(blk.read(lid).c);
}

// Arm 10: threadgroup memory of a tile persists between dispatches in one pass. The first kernel fills it, a draw
// intervenes, the second reads it back.
//@ entry tile_tg_write
kernel void tile_tg_write(threadgroup uint *mem [[threadgroup(0)]],
                          ushort2 tile [[threadgroup_position_in_grid]],
                          ushort tidx [[thread_index_in_threadgroup]]) {
    mem[tidx] = (uint(tile.y) << 24) | (uint(tile.x) << 16) | uint(tidx);
}
//@ entry tile_tg_read
kernel void tile_tg_read(threadgroup uint *mem [[threadgroup(0)]],
                         device uint *out [[buffer(0)]],
                         constant uint &tilesX [[buffer(1)]],
                         ushort2 tile [[threadgroup_position_in_grid]],
                         ushort tidx [[thread_index_in_threadgroup]]) {
    out[(uint(tile.y) * tilesX + tile.x) * 256 + tidx] = mem[tidx];
}


// Translator tests only: persistent tile memory refuses an element with a 16-bit leaf, and a function with an early return.
//@ entry tile_tg_half
kernel void tile_tg_half(threadgroup half *mem [[threadgroup(0)]],
                         device half *out [[buffer(0)]],
                         ushort tidx [[thread_index_in_threadgroup]]) {
    mem[tidx] = half(tidx);
    threadgroup_barrier(mem_flags::mem_threadgroup);
    out[tidx] = mem[tidx ^ 1];
}
//@ entry tile_tg_early
kernel void tile_tg_early(threadgroup uint *mem [[threadgroup(0)]],
                          device uint *out [[buffer(0)]],
                          ushort tidx [[thread_index_in_threadgroup]]) {
    if (tidx > 100) return;
    mem[tidx] = tidx;
    threadgroup_barrier(mem_flags::mem_threadgroup);
    out[tidx] = mem[tidx ^ 1];
}

//@ entry fs_gb2_init
fragment GB2Out fs_gb2_init(float4 pos [[position]]) {
    GB2Out o;
    int x = int(pos.x), y = int(pos.y);
    o.gb.f = float4(float(x), float(y), 0.5, 1.0);
    o.gb.i = x - 10;
    o.gb.u = uint(y * 3);
    o.gb.c = uchar4(uchar(x & 255), uchar(y & 255), uchar(7), uchar(9));
    o.gb.b = uchar((x + y) & 255);
    return o;
}
//@ entry fs_gb2_acc
fragment GB2Out fs_gb2_acc(float4 pos [[position]], GB2 prev [[imageblock_data]]) {
    GB2Out o;
    o.gb = prev;
    o.gb.f += 1.0;
    o.gb.i -= 3;
    o.gb.u += 5u;
    o.gb.c.x += 1;
    o.gb.b += 2;
    return o;
}
//@ entry tile_gb2_dump
kernel void tile_gb2_dump(imageblock<GB2, imageblock_layout_explicit> blk,
                          device uint *out [[buffer(0)]],
                          constant uint &width [[buffer(1)]],
                          ushort2 lid [[thread_position_in_threadgroup]],
                          ushort2 tgsz [[threads_per_threadgroup]],
                          ushort2 tile [[threadgroup_position_in_grid]]) {
    threadgroup_imageblock GB2 *p = blk.data(lid);
    uint2 xy = uint2(tile) * uint2(tgsz) + uint2(lid);
    device uint *o = out + (xy.y * width + xy.x) * 8;
    o[0] = as_type<uint>(p->f.x); o[1] = as_type<uint>(p->f.y); o[2] = as_type<uint>(p->f.z); o[3] = as_type<uint>(p->f.w);
    o[4] = as_type<uint>(p->i); o[5] = p->u; o[6] = as_type<uint>(p->c); o[7] = uint(p->b);
}
