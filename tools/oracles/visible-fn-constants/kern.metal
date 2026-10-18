#include <metal_stdlib>
using namespace metal;
// The shape the system's icon and glass shaders use: a function constant decides whether a linked visible
// function is called, and the function that supplies it is attached at pipeline creation.
constant bool use_custom [[function_constant(0)]];
[[visible]] float custom_fn(float x);
kernel void k(device float *o [[buffer(0)]], uint i [[thread_position_in_grid]])
{
    float v = float(i);
    if (use_custom) v = custom_fn(v);
    o[i] = v;
}
