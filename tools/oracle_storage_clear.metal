// oracle_storage_clear.metal - the game's storage clear, reduced to its shape.
//
// The dispatch under test reaches its target through a device-side chain: an argument buffer whose
// +40 holds a device pointer to a struct whose +8 is a 2D-array texture handle (written), +0 a pointer
// to a struct of the same layout whose handle is zero, and +32 a 3D texture handle that is carried but
// not read. Each 8x8 threadgroup clears a 16x16 tile with four writes per thread (x, y), (x+8, y),
// (x, y+8), (x+8, y+8) to layer 0, level 0, so 128x128 threadgroups cover 2048x2048.
#include <metal_stdlib>
using namespace metal;

struct Foo {
    device Foo *next;                              // +0
    texture2d_array<float, access::write> tex;     // +8
    uint64_t pad16;                                // +16
    uint64_t pad24;                                // +24
    texture3d<float, access::read> vol;            // +32
};

struct AB {
    uint64_t pad[5];                               // +0 .. +32
    device Foo *foo;                               // +40
};

kernel void clear_main(device const AB &ab [[buffer(0)]],
                       uint2 tg [[threadgroup_position_in_grid]],
                       uint2 tid [[thread_position_in_threadgroup]])
{
    device Foo *f = ab.foo;
    uint x = 16u * tg.x + tid.x;
    uint y = 16u * tg.y + tid.y;
    f->tex.write(float4(0.0f), uint2(x, y), 0, 0);
    f->tex.write(float4(0.0f), uint2(x + 8u, y), 0, 0);
    f->tex.write(float4(0.0f), uint2(x, y + 8u), 0, 0);
    f->tex.write(float4(0.0f), uint2(x + 8u, y + 8u), 0, 0);
}
