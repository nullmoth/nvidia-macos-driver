/* NIR backend for curve_math.h: V is a nir_def pointer, ctx is the nir_builder. Include after the NIR headers. */
#ifndef NVK_CURVE_NIR_H
#define NVK_CURVE_NIR_H
#include "curve_math.h"
#define B_(c) ((nir_builder *)(c))
#define D_(v) ((nir_def *)(v))
#define R_(d) ((V)(d))
static V n_c(void *c, float f) { return R_(nir_imm_float(B_(c), f)); }
static V n_add(void *c, V a, V b) { return R_(nir_fadd(B_(c), D_(a), D_(b))); }
static V n_sub(void *c, V a, V b) { return R_(nir_fsub(B_(c), D_(a), D_(b))); }
static V n_mul(void *c, V a, V b) { return R_(nir_fmul(B_(c), D_(a), D_(b))); }
static V n_div(void *c, V a, V b) { return R_(nir_fdiv(B_(c), D_(a), D_(b))); }
static V n_sqrt(void *c, V a) { return R_(nir_fsqrt(B_(c), D_(a))); }
static V n_min(void *c, V a, V b) { return R_(nir_fmin(B_(c), D_(a), D_(b))); }
static V n_max(void *c, V a, V b) { return R_(nir_fmax(B_(c), D_(a), D_(b))); }
static V n_abs(void *c, V a) { return R_(nir_fabs(B_(c), D_(a))); }
static V n_lt(void *c, V a, V b) { return R_(nir_flt(B_(c), D_(a), D_(b))); }
static V n_le(void *c, V a, V b) { return R_(nir_fge(B_(c), D_(b), D_(a))); }
static V n_and(void *c, V a, V b) { return R_(nir_iand(B_(c), D_(a), D_(b))); }
static V n_or(void *c, V a, V b) { return R_(nir_ior(B_(c), D_(a), D_(b))); }
static V n_not(void *c, V a) { return R_(nir_inot(B_(c), D_(a))); }
static V n_sel(void *c, V k, V a, V b) { return R_(nir_bcsel(B_(c), D_(k), D_(a), D_(b))); }
static inline struct cv_ops cv_nir_ops(nir_builder *b)
{
   struct cv_ops o = { b, n_c, n_add, n_sub, n_mul, n_div, n_sqrt, n_min, n_max, n_abs, n_lt, n_le, n_and, n_or, n_not, n_sel };
   return o;
}
#endif
