/* Float backend for curve_math.h: V holds the bits of a float, a bool is 0 or 1. */
#ifndef NVK_CURVE_FLOAT_H
#define NVK_CURVE_FLOAT_H
#include "curve_math.h"
#include <math.h>
#include <string.h>
static inline float fv_(V v) { uint32_t b = (uint32_t)v; float f; memcpy(&f, &b, 4); return f; }
static inline V vf_(float f) { uint32_t b; memcpy(&b, &f, 4); return b; }
static V f_c(void *c, float f) { (void)c; return vf_(f); }
static V f_add(void *c, V a, V b) { (void)c; return vf_(fv_(a) + fv_(b)); }
static V f_sub(void *c, V a, V b) { (void)c; return vf_(fv_(a) - fv_(b)); }
static V f_mul(void *c, V a, V b) { (void)c; return vf_(fv_(a) * fv_(b)); }
static V f_div(void *c, V a, V b) { (void)c; return vf_(fv_(a) / fv_(b)); }
static V f_sqrt(void *c, V a) { (void)c; return vf_(sqrtf(fv_(a))); }
static V f_min(void *c, V a, V b) { (void)c; return vf_(fminf(fv_(a), fv_(b))); }
static V f_max(void *c, V a, V b) { (void)c; return vf_(fmaxf(fv_(a), fv_(b))); }
static V f_abs(void *c, V a) { (void)c; return vf_(fabsf(fv_(a))); }
static V f_lt(void *c, V a, V b) { (void)c; return fv_(a) < fv_(b); }
static V f_le(void *c, V a, V b) { (void)c; return fv_(a) <= fv_(b); }
static V f_and(void *c, V a, V b) { (void)c; return a && b; }
static V f_or(void *c, V a, V b) { (void)c; return a || b; }
static V f_not(void *c, V a) { (void)c; return !a; }
static V f_sel(void *c, V k, V a, V b) { (void)c; return k ? a : b; }
static const struct cv_ops cv_float_ops = { 0, f_c, f_add, f_sub, f_mul, f_div, f_sqrt, f_min, f_max, f_abs, f_lt, f_le, f_and, f_or, f_not, f_sel };
#endif
