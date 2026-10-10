/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#pragma once
#include <stdint.h>
#include <stddef.h>
const char *nvmtl_image_ident(const void *addr, char *out, size_t n);
typedef struct { void *buf; void *mem; void *map; size_t size; size_t alloc; int placed; void *pool; size_t poff; int imported; int bar1; size_t moff; int sparse, sysmem; } nvk_buffer;
#include "nvmtl_sample_positions.h"
typedef struct { void *img; void *mem; void *view; uint32_t w, h; size_t alloc; uint32_t layout; uint32_t mips; uint32_t fmt, bpp; uint32_t storage, mtl_type, layers; int placed; uint32_t bw, bh; uint32_t samples; uint32_t sysmem; uint32_t vkflags, usage;  uint32_t vpair;  } nvk_image;
#define NVMTL_NVAR 16
#define NVMTL_NVAR_MAX 4096
#define NVMTL_NCOL 8
#define NVMTL_RPX_MAX  1020
#define NVMTL_RPX_GROW 4
#define NVMTL_LOAD_COLOUR(i) ((i) ? (1u << ((i) + 2)) : 1u)
#define NVMTL_LOAD_DEPTH 2u
#define NVMTL_LOAD_STENCIL 4u
#define NVMTL_LOAD_ALL 0x3ffu
typedef struct { void *rp; void *pipe; void *layout; int has_depth; uint32_t dfmt; uint32_t stex_hi[2];
    void *rpv[4]; uint32_t cfmt;
    void *mod; uint32_t nvar, capvar; struct nvmtl_cvar { uint32_t l[3]; void *pipe; } *var;
    uint32_t vin_mask;
    uint32_t samples;
    uint32_t tess, tess_n, tess_cpidx, tess_nvb, tess_vb[8];
    uint32_t ncol, cfmts[NVMTL_NCOL], has_stencil, rmask;
    void **rpx; uint32_t *rpxk, nrpx, crpx;
    uint32_t vin_dyn, vin_stride[32];
    uint64_t bmask[4]; uint32_t bmask_ok;
    uint64_t gmask[2][4]; uint32_t gmask_ok;
    uint32_t ag_mask, ag_req; void *ag_src; size_t ag_srcn; void *ag_safe;
    uint64_t shape_identity;
    uint32_t lin;
    uint64_t tbm[2][4]; uint32_t tbany;
} nvk_pipeline;
void nvmtl_vk_pipeline_destroy(nvk_pipeline *p);
typedef struct { void *pool; nvk_buffer stage; } nvk_queue;
#include "nvmtl_sampler_desc.h"
typedef struct { void *s; int linear, repeat; float lod_min, lod_max; nvmtl_sampler_desc desc; int full_state; } nvk_sampler;
typedef struct { void *pool, *set; } nvk_setref;
typedef struct {
    uint64_t gpu_tick, uptime_ns, uncertainty_ns;
    uint32_t valid_bits;
    double period_ns;
    unsigned valid;
} nvmtl_gpu_clock;
typedef struct {
    nvmtl_gpu_clock clock;
    uint64_t start_tick, end_tick, host_before_ns, host_after_ns;
    unsigned valid;
} nvmtl_gpu_interval;
void nvmtl_vk_clock_refresh(void);
int nvmtl_vk_interval_seconds(const nvmtl_gpu_interval *t, double *start, double *end);

typedef struct { void *cb; void *fb; void *pool; void *dset[2]; void *depth_view; void *table;
                 void **fbs; unsigned nfb, capfb;
                 nvk_setref *sets; unsigned nsets, capsets;
                 void *address_pages; unsigned address_count;
                 unsigned nobind, skipped;
                 void *dcache[32]; void *dcachepool; unsigned ndcache;
                 unsigned in_rp, occlusion_reset; uint32_t wseq;
                 void *occlusion_pages; uint32_t occlusion_count;
                 void *timing_pool; nvmtl_gpu_interval timing;
                 void *pt_pool; unsigned pt_n, pt_open; uint32_t pt_w[128], pt_h[128], pt_f[128]; uint32_t pt_fs[128][4];
                 void *counter_recording; unsigned counter_error;
                 nvmtl_sample_pattern sample_pattern;
                 void **rvs; unsigned nrv, caprv;
               } nvk_cmdbuf;
int  nvmtl_vk_init(void);
int nvmtl_vk_sample_positions_backend(void);
int nvmtl_vk_sample_positions_default(uint32_t count, nvmtl_sample_pattern *out);
int  nvmtl_vk_cmd_abandon(nvk_cmdbuf *c);
void nvmtl_vk_cmd_counts(uint64_t *begun, uint64_t *submitted, uint64_t *abandoned);
const char *nvmtl_vk_device_name(void);
void nvmtl_vk_limits(uint32_t *layers, uint32_t *shared_bytes, uint32_t wg[3], uint32_t *wg_inv, uint32_t *sm);
int nvmtl_vk_tg_reject(const uint32_t l[3], const uint32_t *t, const char *kernel, uint32_t lin);
void nvmtl_vk_pubcaps(uint32_t *color_samples, uint32_t *depth_samples, uint64_t *max_storage_range, uint64_t *linear_align);
int  nvmtl_vk_buffer_create(size_t size, int host_visible, nvk_buffer *out);
enum { NVP_BEGIN, NVP_BUFNEW, NVP_BUFPOOL, NVP_BUFDEL, NVP_HEAP, NVP_SYNCIN, NVP_SYNCOUT, NVP_COPY, NVP_N };
uint64_t nvmtl_perf_now(void);
void nvmtl_perf_note(int what, uint64_t t0, uint64_t bytes);
int  nvmtl_vk_buffer_import_host(void *ptr, size_t size, nvk_buffer *out);
size_t nvmtl_vk_host_import_align(void);
#define NVMTL_MAX_GEOMS 256
typedef struct { void *as; uint64_t addr; size_t size; int instance; } nvk_accel;
typedef struct { uint64_t vertices; uint32_t stride; uint32_t count; uint32_t vertex_format; uint32_t max_vertex;
                 uint64_t indices; uint32_t index_type; uint32_t opaque;
                 uint64_t boxes; uint32_t box_stride; uint32_t is_boxes; } nvmtl_geom;
int  nvmtl_vk_rt_available(void);
int  nvmtl_vk_accel_sizes(const nvmtl_geom *g, uint32_t ngeom, int instance, uint32_t instance_count, size_t *as_size, size_t *scratch_size);
int  nvmtl_vk_accel_create(nvk_buffer *backing, size_t size, int instance, nvk_accel *out);
void nvmtl_vk_accel_destroy(nvk_accel *a);
int  nvmtl_vk_cmd_build_accel(nvk_cmdbuf *c, nvk_accel *dst, const nvmtl_geom *g, uint32_t ngeom, uint64_t instances_addr, uint32_t instance_count, uint64_t scratch_addr);
int  nvmtl_vk_cmd_copy_accel(nvk_cmdbuf *c, nvk_accel *src, nvk_accel *dst);
int  nvmtl_vk_convert_instances(const void *mtl_instances, uint32_t stride, uint32_t count, const uint64_t *as_addrs, uint32_t n_as, void *vk_instances_out,
                                uint32_t mtl_type,
                                uint32_t *uid_out,
                                const uint32_t *as_ift);
int  nvmtl_vk_image_create(uint32_t w, uint32_t h, nvk_image *out);
int  nvmtl_vk_image_create_fmt(uint32_t w, uint32_t h, int bgra, nvk_image *out);
int  nvmtl_vk_pipeline_create(const void *vs, size_t vn, const void *fs, size_t fn, nvk_pipeline *out);
int  nvmtl_vk_pipeline_create_fmt(const void *vs, size_t vn, const void *fs, size_t fn, int bgra, nvk_pipeline *out);
int  nvmtl_vk_pipeline_create_depth(const void *vs, size_t vn, const void *fs, size_t fn, int bgra, int hasDepth, nvk_pipeline *out);
int  nvmtl_vk_depth_create(uint32_t w, uint32_t h, nvk_image *out);
void nvmtl_vk_cmd_set_depth(nvk_cmdbuf *c, int test, int write, int compare);
int  nvmtl_vk_queue_create(nvk_queue *out);
int  nvmtl_vk_cmd_begin(nvk_queue *q, nvk_cmdbuf *out);
int  nvmtl_vk_cmd_begin_render(nvk_cmdbuf *c, nvk_image *img, nvk_pipeline *p, const float clear[4]);
int  nvmtl_vk_cmd_begin_render_ex(nvk_cmdbuf *c, nvk_image *img, nvk_pipeline *p, const float clear[4], uint32_t load);
int  nvmtl_vk_cmd_begin_render_ms(nvk_cmdbuf *c, nvk_image *img, nvk_pipeline *p, const float clear[4], uint32_t load, nvk_image *resolve);
uint64_t nvmtl_vk_working_set(void);
uint64_t nvmtl_vk_vram_bytes(void);
void     nvmtl_vk_mem_split(uint64_t *vram, uint64_t *sys);
int  nvmtl_vk_image_create_ms(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp, uint32_t samples, nvk_image *out);
int  nvmtl_vk_image_create_ms_full(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp, uint32_t samples, uint32_t mtl_type, uint32_t layers, nvk_image *out);
int  nvmtl_vk_depth_create_ms(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t aspect, uint32_t samples, nvk_image *out);
int  nvmtl_vk_pipeline_create_vin_ms(const void *vs, size_t vn, const void *fs, size_t fn, uint32_t vkfmt, uint32_t depthfmt, const void *blend, const void *vin, uint32_t samples, void *out);
void nvmtl_vk_cmd_set_viewport(nvk_cmdbuf *c, float x, float y, float w, float h, float zn, float zf);
void nvmtl_vk_cmd_draw(nvk_cmdbuf *c, uint32_t first, uint32_t count, uint32_t instances, uint32_t baseInstance);
int  nvmtl_vk_cmd_clear_image(nvk_cmdbuf *c, nvk_image *img, const float clear[4]);
void nvmtl_vk_cmd_set_scissor(nvk_cmdbuf *c, int32_t x, int32_t y, uint32_t w, uint32_t h);
void nvmtl_vk_cmd_set_cull(nvk_cmdbuf *c, int mode);
int  nvmtl_vk_cmd_set_topology(nvk_cmdbuf *c, unsigned mtlType);
void nvmtl_vk_cmd_draw_indexed(nvk_cmdbuf *c, nvk_buffer *idx, size_t offset, uint32_t count, int is32,
                               uint32_t instances, int32_t baseVertex, uint32_t baseInstance);
#define NVMTL_NQUERY 128
#define NVMTL_NQUERY_WORDS ((NVMTL_NQUERY + 63) / 64)
#define NVMTL_NQUERY_NOANS 32
int nvmtl_vk_cmd_sum_occlusion(nvk_cmdbuf *c, nvk_buffer *src, size_t first, uint32_t count, nvk_buffer *dst, size_t offset);
int nvmtl_vk_cmd_alloc_occlusion(nvk_cmdbuf *c, uint32_t *query);
void nvmtl_vk_cmd_retire_occlusion(nvk_cmdbuf *c);
void *nvmtl_vk_ts_pool_create(uint32_t count);
void nvmtl_vk_ts_pool_destroy(void *pool);
int  nvmtl_vk_cmd_ts_write(nvk_cmdbuf *c, void *pool, uint32_t idx, int barrier);
int  nvmtl_vk_cmd_ts_resolve(nvk_cmdbuf *c, void *pool, uint32_t first, uint32_t count, nvk_buffer *dst, size_t off);
int  nvmtl_vk_ts_read(void *pool, uint32_t first, uint32_t count, uint64_t *out);
int  nvmtl_vk_gpu_timestamp(uint64_t *cpu_ns, uint64_t *gpu);
int  nvmtl_vk_cmd_begin_occlusion(nvk_cmdbuf *c, uint32_t idx);
void nvmtl_vk_cmd_end_occlusion(nvk_cmdbuf *c, uint32_t idx);
int  nvmtl_vk_cmd_copy_occlusion(nvk_cmdbuf *c, uint32_t first, uint32_t count, nvk_buffer *dst, size_t off);
int  nvmtl_vk_have_polygon_mode(void);
int  nvmtl_vk_cmd_set_depth_clip(nvk_cmdbuf *c, int clip);
int  nvmtl_vk_cmd_set_polygon_mode(nvk_cmdbuf *c, int line);
void nvmtl_vk_cmd_set_blend_color(nvk_cmdbuf *c, const float rgba[4]);
void nvmtl_vk_cmd_set_stencil_ref(nvk_cmdbuf *c, uint32_t front, uint32_t back);
void nvmtl_vk_cmd_set_depth_bias(nvk_cmdbuf *c, float constant, float slope, float clamp);
void nvmtl_vk_cmd_set_line_width(nvk_cmdbuf *c, float w);
int  nvmtl_vk_fill_buffer(nvk_queue *q, nvk_buffer *b, size_t offset, size_t size, uint8_t value);
int  nvmtl_vk_bind_buffer(nvk_cmdbuf *c, uint32_t set, uint32_t index, nvk_buffer *b, size_t offset);
int  nvmtl_vk_trace_buffers(nvk_cmdbuf *c, char *out, size_t cap);
int  nvmtl_vk_bind_buffer_offset(nvk_cmdbuf *c, uint32_t set, uint32_t index, size_t offset);
int  nvmtl_vk_bind_texture(nvk_cmdbuf *c, uint32_t set, uint32_t index, nvk_image *img);
int  nvmtl_vk_sampler_create(int linear, int repeat, nvk_sampler *out);
int  nvmtl_vk_sampler_create_desc(const nvmtl_sampler_desc *desc, nvk_sampler *out);
void nvmtl_vk_sampler_destroy(nvk_sampler *s);
int  nvmtl_vk_bind_sampler(nvk_cmdbuf *c, uint32_t set, uint32_t index, nvk_sampler *s);
void nvmtl_vk_cmd_bind_set(nvk_cmdbuf *c, nvk_pipeline *p);
int  nvmtl_vk_compute_pipeline_create(const void *cs, size_t n, nvk_pipeline *out);
void nvmtl_vk_cmd_dispatch(nvk_cmdbuf *c, nvk_pipeline *p, uint32_t gx, uint32_t gy, uint32_t gz);
void nvmtl_vk_cmd_dispatch_threads(nvk_cmdbuf *c, nvk_pipeline *p, const uint32_t threads[3], const uint32_t tg[3]);
void nvmtl_vk_cmd_dispatch_indirect_tg(nvk_cmdbuf *c, nvk_pipeline *p, nvk_buffer *b, size_t off, const uint32_t tg[3]);
void nvmtl_vk_cmd_end_render(nvk_cmdbuf *c);
int  nvmtl_vk_submit_wait_at(nvk_cmdbuf *c, const char *who);
typedef struct nvmtl_inflight nvmtl_inflight;
nvmtl_inflight *nvmtl_vk_inflight_new(void);
void nvmtl_vk_inflight_free(nvmtl_inflight *f);
int nvmtl_vk_image_reback(nvk_image *i, int to_sysmem, int a8);
void nvmtl_vk_res1_stats(uint64_t out[6]);
int nvmtl_vk_image_migrate(nvk_image *i, int to_sysmem, int a8, nvk_image *old_out);
void nvmtl_vk_image_free_backing(nvk_image *old);
int nvmtl_vk_vram_room(uint64_t want);
uint64_t nvmtl_vk_vram_free_now(void);
uint64_t nvmtl_vk_vram_headroom(void);
void nvmtl_vk_bindless_rewrite(uint32_t slot, void *view);
void nvmtl_vk_res2_stats(uint64_t out[5]);
int nvmtl_vk_buffer_migrate(nvk_buffer *b, int to_sysmem);
void nvmtl_vk_res3_stats(uint64_t out[6]);
int  nvmtl_vk_submit_begin_at(nvk_cmdbuf *c, nvmtl_inflight *f, const char *who);
#define nvmtl_vk_submit_wait(c) nvmtl_vk_submit_wait_at((c), __func__)
#define nvmtl_vk_submit_begin(c, f) nvmtl_vk_submit_begin_at((c), (f), __func__)
int  nvmtl_vk_submit_finish(nvmtl_inflight *f);
void nvmtl_vk_gsub_nvk_stats(uint64_t *out, unsigned n);
int nvmtl_vk_pre_open(void);
int nvmtl_vk_pre_copy(nvk_buffer *src, size_t srcOff, nvk_buffer *dst, size_t dstOff, size_t size);
int nvmtl_vk_pre_fill(nvk_buffer *b, size_t offset, size_t size, uint32_t word);
void nvmtl_vk_pre_hold(void *retained);
extern void (*nvmtl_pre_release_hook)(void *);
extern void (*nvmtl_pre_submit_hook)(void);
int  nvmtl_vk_image_read(nvk_queue *q, nvk_image *img, void *dst, size_t row_bytes);
int  nvmtl_vk_image_write(nvk_queue *q, nvk_image *img, const void *src, size_t row_bytes);
int  nvmtl_vk_copy_buffer(nvk_queue *q, nvk_buffer *src, size_t srcOff, nvk_buffer *dst, size_t dstOff, size_t size);
int  nvmtl_vk_image_create_mips(uint32_t w, uint32_t h, int bgra, uint32_t mips, nvk_image *out);
int  nvmtl_vk_image_create_ex(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp, int a8, uint32_t mips, nvk_image *out);
int  nvmtl_vk_image_create_ex2(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp, int a8, uint32_t mips, int storage, nvk_image *out);
int  nvmtl_vk_format_is_storage(uint32_t vkfmt);
int  nvmtl_vk_bind_storage_view(nvk_cmdbuf *c, uint32_t set, uint32_t index, void *view);
uint32_t nvmtl_vk_bindless_put(void *view);
void     nvmtl_vk_bindless_drop(uint32_t slot);
int  nvmtl_vk_pipeline_create_ex(const void *vs, size_t vn, const void *fs, size_t fn, uint32_t vkfmt, int hasDepth, nvk_pipeline *out);
int  nvmtl_vk_image_view_create_ex(nvk_image *img, uint32_t vkfmt, int a8, uint32_t baseLevel, uint32_t levelCount, void **out_view);
int  nvmtl_vk_depth_create_ex(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t aspect, nvk_image *out);
typedef struct { uint32_t enabled, src_rgb, dst_rgb, op_rgb, src_alpha, dst_alpha, op_alpha, write_mask; } nvk_blend_state;
int nvmtl_vk_pipeline_create_blend(const void *vs, size_t vn, const void *fs, size_t fn, uint32_t vkfmt, uint32_t depthfmt, const nvk_blend_state *blend, nvk_pipeline *out);
#define NVMTL_NVIN 31
_Static_assert(NVMTL_NVIN <= 32, "nvk_pipeline.vin_stride and every vin mask hold 32 buffer indices");
#define NVMTL_STRIDE_DYNAMIC 0xFFFFFFFFu
typedef struct { uint32_t nattr; struct { uint32_t location, buffer, mtlfmt, offset; } attr[NVMTL_NVIN];
                 struct { uint32_t stride, step, rate; } layout[NVMTL_NVIN]; } nvk_vertex_input;
#define NVMTL_TESS_DESC_DEFINED
typedef struct { uint32_t patch_type, n, partition, winding, stepfn, scale_en, cpidx; float maxf; int32_t inst_loc; } nvk_tess_desc;
int nvmtl_vk_pipeline_create_tess(const void *vs, size_t vn, const void *fs, size_t fn, uint32_t vkfmt, uint32_t depthfmt, const void *blend,
                                  const void *vin, uint32_t samples, const nvk_tess_desc *td, void *out);
int nvmtl_spirv_tess_info(const void *spv, size_t bytes, uint32_t *patchType, uint32_t *cps);
typedef struct { uint32_t ncol, cfmt[NVMTL_NCOL]; nvk_blend_state blend[NVMTL_NCOL]; uint32_t dfmt, discard, a2c, a2one; } nvk_rt;
typedef struct { nvk_image *col[NVMTL_NCOL]; void *colview[NVMTL_NCOL]; uint32_t colw[NVMTL_NCOL], colh[NVMTL_NCOL];
    nvk_image *res[NVMTL_NCOL]; void *resview[NVMTL_NCOL]; uint32_t resw[NVMTL_NCOL], resh[NVMTL_NCOL];
    float clear[NVMTL_NCOL][4]; nvk_image *ds; void *dsview; float clear_depth; uint32_t clear_stencil, load, w, h;
    uint32_t layers; nvk_image *dres; void *dresview; uint32_t dresmode, sresmode;
    nvmtl_sample_pattern sample_pattern; uint32_t depth_store_options, stencil_store_options;
    uint32_t ds_level, ds_layer;
    uint32_t colfmt[NVMTL_NCOL], resfmt[NVMTL_NCOL];
    uint32_t col_level[NVMTL_NCOL], col_layer[NVMTL_NCOL];  } nvk_pass;
typedef struct { uint32_t ncol, cfmts[NVMTL_NCOL], has_depth, dfmt, samples; } nvk_rtsig;
int  nvmtl_vk_pipeline_create_rt(const void *vs, size_t vn, const void *fs, size_t fn, const nvk_rt *rt, const void *vin, uint32_t samples,
                                 const nvk_tess_desc *td, void *out);
int  nvmtl_vk_cmd_begin_pass(nvk_cmdbuf *c, const nvk_pass *ps, nvk_pipeline *p);
int  nvmtl_vk_cmd_empty_pass(nvk_cmdbuf *c, const nvk_pass *ps);
int  nvmtl_vk_pass_kind(const nvk_pass *ps);
void nvmtl_vk_cmd_set_stencil(nvk_cmdbuf *c, int enable, const uint32_t *front6, const uint32_t *back6);
int  nvmtl_vk_cmd_retire_view(nvk_cmdbuf *c, void *view);
int  nvmtl_vk_cmd_attachment_view(nvk_cmdbuf *c, nvk_image *img, int depth, uint32_t level, uint32_t slice, void **out, uint32_t *w, uint32_t *h);
int  nvmtl_vk_depth_create_full(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t aspect, uint32_t samples, uint32_t mtl_type,
                                uint32_t layers, uint32_t mips, nvk_image *out);
int  nvmtl_vk_dual_src(void);
int  nvmtl_vk_texel_on(void);
int  nvmtl_vk_texel_view_create(nvk_buffer *b, size_t offset, size_t range, uint32_t vkfmt, int storage, void **alias, void **view, int *storage_ok);
void nvmtl_vk_texel_view_destroy(void *alias, void *view);
int  nvmtl_vk_bind_texel_view(nvk_cmdbuf *c, uint32_t set, uint32_t index, void *view, int storage);
int  nvmtl_spirv_has_index1(const void *p, size_t n);
int  nvmtl_vk_cmd_attachment_view_n(nvk_cmdbuf *c, nvk_image *img, int depth, uint32_t level, uint32_t slice, uint32_t nlayers,
                                    uint32_t vkfmt, int a8, void **out, uint32_t *w, uint32_t *h);
int  nvmtl_vk_image_view_create_range_swz(nvk_image *img, uint32_t vkfmt, int a8, uint32_t baseLevel, uint32_t levelCount, uint32_t baseLayer,
                                          uint32_t layerCount, uint32_t mtl_view_type, const uint8_t mtl_swz[4], void **out_view);
void nvmtl_vk_pipeline_sig(const nvk_pipeline *p, nvk_rtsig *out);
int  nvmtl_vk_pipeline_compatible(const nvk_rtsig *sig, const nvk_pipeline *p);
int nvmtl_vk_cmd_draw_patches(nvk_cmdbuf *c, const nvk_pipeline *p, uint64_t fac, uint32_t istride, float scale, uint64_t cpx, uint64_t pix,
                              uint32_t start, uint32_t count, uint32_t instances, uint32_t baseInstance, nvk_buffer *ind, size_t indoff);
int nvmtl_vk_pipeline_create_vin(const void *vs, size_t vn, const void *fs, size_t fn, uint32_t vkfmt, uint32_t depthfmt, const nvk_blend_state *blend,
                                 const nvk_vertex_input *vin, nvk_pipeline *out);
int nvmtl_vk_cmd_vertex_inputs(nvk_cmdbuf *c, const nvk_pipeline *p, const uint32_t *vstride, uint32_t vset);
int  nvmtl_vk_pipeline_create_ex2(const void *vs, size_t vn, const void *fs, size_t fn, uint32_t vkfmt, uint32_t depthfmt, nvk_pipeline *out);
void nvmtl_vk_cmd_bind_pipeline(nvk_cmdbuf *c, nvk_pipeline *p);
int  nvmtl_vk_image_gen_mipmaps(nvk_queue *q, nvk_image *img);
int  nvmtl_vk_image_view_create(nvk_image *img, int bgra, uint32_t baseLevel, uint32_t levelCount, void **out_view);
void nvmtl_vk_image_view_destroy(void *view);
int  nvmtl_vk_bind_texture_view(nvk_cmdbuf *c, uint32_t set, uint32_t index, void *view);
int  nvmtl_vk_image_write_region_level(nvk_queue *q, nvk_image *img, const void *src, size_t row_bytes,
                                       uint32_t x, uint32_t y, uint32_t w, uint32_t h, uint32_t level);
int  nvmtl_vk_image_read_region_level(nvk_queue *q, nvk_image *img, void *dst, size_t row_bytes,
                                      uint32_t x, uint32_t y, uint32_t w, uint32_t h, uint32_t level);
int  nvmtl_vk_image_write_region(nvk_queue *q, nvk_image *img, const void *src, size_t row_bytes,
                                 uint32_t x, uint32_t y, uint32_t w, uint32_t h);
int  nvmtl_vk_image_read_region(nvk_queue *q, nvk_image *img, void *dst, size_t row_bytes,
                                uint32_t x, uint32_t y, uint32_t w, uint32_t h);
int  nvmtl_vk_copy_image(nvk_queue *q, nvk_image *src, uint32_t sx, uint32_t sy,
                         nvk_image *dst, uint32_t dx, uint32_t dy, uint32_t w, uint32_t h);
int  nvmtl_vk_copy_buffer_to_image(nvk_queue *q, nvk_buffer *b, size_t off, uint32_t row_bytes,
                                   nvk_image *img, uint32_t x, uint32_t y, uint32_t w, uint32_t h);
int  nvmtl_vk_copy_image_to_buffer(nvk_queue *q, nvk_image *img, uint32_t x, uint32_t y, uint32_t w, uint32_t h,
                                   nvk_buffer *b, size_t off, uint32_t row_bytes);
void nvmtl_vk_cmd_draw_indirect(nvk_cmdbuf *c, nvk_buffer *b, size_t off);
void nvmtl_vk_cmd_draw_indexed_indirect(nvk_cmdbuf *c, nvk_buffer *idx, size_t idxOff, int is32, nvk_buffer *b, size_t off);
void nvmtl_vk_cmd_dispatch_indirect(nvk_cmdbuf *c, nvk_pipeline *p, nvk_buffer *b, size_t off);
void nvmtl_vk_cmd_barrier(nvk_cmdbuf *c);
void nvmtl_vk_buffer_destroy(nvk_buffer *b);
void nvmtl_vk_image_destroy(nvk_image *i);
void nvmtl_vk_queue_destroy(nvk_queue *q);
size_t nvmtl_vk_allocated_bytes(void);
uint64_t nvmtl_vk_buffer_address(nvk_buffer *b);

int nvmtl_vk_image_create_typed(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp, int a8, uint32_t mips, int storage, uint32_t mtl_type, uint32_t layers, nvk_image *out);
typedef struct { void *mem; void *map; size_t size; uint32_t memtype; int host_visible; } nvk_heap;
int  nvmtl_vk_heap_create(size_t size, int host_visible, nvk_heap *out);
int  nvmtl_vk_format_block(uint32_t vkfmt, uint32_t *bw, uint32_t *bh, uint32_t *block_bytes);
static inline uint32_t nvmtl_vk_layers_for_type(uint32_t t, uint32_t n) { n = n ? n : 1; return (t == 1 || t == 3 || t == 8) ? n : t == 5 ? 6u : t == 6 ? 6u * n : 1u; }
int nvmtl_vk_cmd_prepare_sampled_image(nvk_cmdbuf *c, nvk_image *img, int storage);
int  nvmtl_vk_image_view_create_range(nvk_image *img, uint32_t vkfmt, int a8, uint32_t baseLevel, uint32_t levelCount, uint32_t baseLayer, uint32_t layerCount, uint32_t mtl_view_type, void **out_view);
int  nvmtl_vk_image_view_create_range_aspect(nvk_image *img, uint32_t vkfmt, int a8, uint32_t aspect, uint32_t baseLevel, uint32_t levelCount, uint32_t baseLayer, uint32_t layerCount, uint32_t mtl_view_type, void **out_view);
int  nvmtl_vk_image_write_region_level_layer(nvk_queue *q, nvk_image *img, const void *src, size_t row_bytes, uint32_t x, uint32_t y, uint32_t w, uint32_t h, uint32_t level, uint32_t layer);
int  nvmtl_vk_image_read_region_level_layer(nvk_queue *q, nvk_image *img, void *dst, size_t row_bytes, uint32_t x, uint32_t y, uint32_t w, uint32_t h, uint32_t level, uint32_t layer);
int  nvmtl_vk_cmd_copy_image_sub(nvk_cmdbuf *c, nvk_image *src, uint32_t sl, uint32_t ss, uint32_t sx, uint32_t sy, nvk_image *dst, uint32_t dl, uint32_t ds, uint32_t dx, uint32_t dy, uint32_t w, uint32_t h);
int  nvmtl_vk_cmd_copy_image_sub3(nvk_cmdbuf *c, nvk_image *src, uint32_t sl, uint32_t ss, uint32_t sx, uint32_t sy, uint32_t sz, nvk_image *dst, uint32_t dl, uint32_t ds, uint32_t dx, uint32_t dy, uint32_t dz, uint32_t w, uint32_t h, uint32_t depth);
int  nvmtl_vk_image_view_create_aspect(nvk_image *img, uint32_t vkfmt, uint32_t aspect, uint32_t baseLevel, uint32_t levelCount, void **out_view);
void nvmtl_vk_heap_destroy(nvk_heap *h);
int  nvmtl_vk_buffer_create_placed(size_t size, nvk_heap *heap, size_t offset, nvk_buffer *out);
int  nvmtl_vk_image_create_buffer_alias(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp, int a8, int storage, int render,
                                        const nvk_buffer *b, size_t offset, size_t row_pitch, nvk_image *out);
int  nvmtl_vk_image_placed_fits(const nvk_image *img, size_t len);
int  nvmtl_vk_image_create_typed_placed(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp, int a8, uint32_t mips, int storage, uint32_t mtl_type, uint32_t layers, nvk_heap *heap, size_t offset, nvk_image *out);
int  nvmtl_vk_depth_create_ex_placed(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t aspect, nvk_heap *heap, size_t offset, nvk_image *out);
int  nvmtl_vk_buffer_size_align(size_t size, size_t *out_size, size_t *out_align);
int  nvmtl_vk_image_size_align(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t mips, uint32_t mtl_type, uint32_t layers, int depth_attachment, int storage, size_t *out_size, size_t *out_align);
int nvmtl_vk_copy_buffer_to_image_layer(nvk_queue *q, nvk_buffer *b, size_t off, uint32_t row_bytes,
                                  nvk_image *img, uint32_t x, uint32_t y, uint32_t w, uint32_t h, uint32_t layer);
int nvmtl_vk_copy_image_to_buffer_layer(nvk_queue *q, nvk_image *img, uint32_t x, uint32_t y, uint32_t w, uint32_t h,
                                  nvk_buffer *b, size_t off, uint32_t row_bytes, uint32_t layer);

int nvmtl_vk_cmd_fill_buffer(nvk_cmdbuf *c, nvk_buffer *b, size_t offset, size_t size, uint8_t value);
int nvmtl_vk_cmd_write_word(nvk_cmdbuf *c, nvk_buffer *b, size_t offset, uint32_t word);

int nvmtl_vk_cmd_copy_buffer(nvk_cmdbuf *c, nvk_buffer *src, size_t srcOff, nvk_buffer *dst, size_t dstOff, size_t size);

int nvmtl_vk_cmd_copy_image(nvk_cmdbuf *c, nvk_image *src, uint32_t sx, uint32_t sy,
                        nvk_image *dst, uint32_t dx, uint32_t dy, uint32_t w, uint32_t h);

void nvmtl_vk_copy_aspect_override(uint32_t aspect, uint32_t bpp);
int nvmtl_vk_cmd_copy_buffer_to_image_level_layer(nvk_cmdbuf *c, nvk_buffer *b, size_t off, uint32_t row_bytes, nvk_image *img, uint32_t x, uint32_t y, uint32_t w, uint32_t h, uint32_t level, uint32_t layer);
int nvmtl_vk_cmd_copy_image_to_buffer_level_layer(nvk_cmdbuf *c, nvk_image *img, uint32_t x, uint32_t y, uint32_t w, uint32_t h, nvk_buffer *b, size_t off, uint32_t row_bytes, uint32_t level, uint32_t layer);
int nvmtl_vk_cmd_copy_buffer_to_image_layer(nvk_cmdbuf *c, nvk_buffer *b, size_t off, uint32_t row_bytes,
                                  nvk_image *img, uint32_t x, uint32_t y, uint32_t w, uint32_t h, uint32_t layer);

int nvmtl_vk_cmd_copy_image_to_buffer_layer(nvk_cmdbuf *c, nvk_image *img, uint32_t x, uint32_t y, uint32_t w, uint32_t h,
                                  nvk_buffer *b, size_t off, uint32_t row_bytes, uint32_t layer);

int nvmtl_vk_cmd_image_gen_mipmaps(nvk_cmdbuf *c, nvk_image *img);

int nvmtl_vk_surface_share_on(void);
int nvmtl_vk_surface_vram(uint32_t surfaceID, uint32_t plane, size_t need, nvk_buffer *out);
int nvmtl_vk_surface_dirty(uint32_t surfaceID, uint32_t plane);
