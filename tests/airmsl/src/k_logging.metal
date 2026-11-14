#include <metal_stdlib>
#include <metal_logging>
using namespace metal;
kernel void k_log(device float *o [[buffer(0)]], uint i [[thread_position_in_grid]]) {
  os_log_default.log_info("value %f", o[i]); o[i] += 1.0f; }
