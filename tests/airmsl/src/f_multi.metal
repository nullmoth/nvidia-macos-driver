#include <metal_stdlib>
using namespace metal;
struct O { float4 a [[color(0)]]; uint b [[color(1)]]; float d [[depth(any)]]; uint m [[sample_mask]]; };
fragment O f_multi(float4 p [[position]], uint sid [[sample_id]], bool front [[front_facing]]) { O o; o.a = p; o.b = sid; o.d = p.z; o.m = front ? 0xf : 0x1; return o; }
