#include <metal_stdlib>
using namespace metal;
struct Node { device float *data; device Node *next; };
kernel void k_bp(device Node *root [[buffer(0)]], uint i [[thread_position_in_grid]]) {
  device Node *n = root; float s = 0; for (int j = 0; j < 4 && n; ++j) { s += n->data[i]; n = n->next; } root->data[i] = s; }
