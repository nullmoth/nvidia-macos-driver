/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#pragma once
#include <stdint.h>
#include <stddef.h>
const char *nvmtl_image_ident(const void *addr, char *out, size_t n);
// bind_lost: a sparse rebind AND its restore were both refused, so we do not know which allocation the
// page tables name. Neither is safe to free, so the destroy path keeps both and counts the bytes.
typedef struct { void *buf; void *mem; void *map; size_t size; size_t alloc; int placed; void *pool; size_t poff; int imported; int bar1; size_t moff; int sparse, sysmem; int mtype1; /* memory type + 1 when known */
                 uint32_t reg; /* handle in the live GPU-range registry (nvmtl_live.h), 0 when unregistered */
                 int fresh; /* host-visible memory from its own vkAllocateMemory: zeroed by the kernel, never recycled */
                 uint64_t parked_ns; /* when the buffer park took it (CLOCK_UPTIME_RAW) */  void *lost_mem; int bind_lost;
                 uint8_t hz_ut; /* hazard model: 0 not yet known, 1 tracked, 2 untracked (Metal hazardTrackingMode) */ } nvk_buffer;
#include "nvmtl_sample_positions.h"
#include "nvmtl_fault_context.h"
#include "nvmtl_page_life.h"
#include "nvmtl_live.h"
#include "nvmtl_slots.h"
#include "nvmtl_nvk_layout.h"
typedef struct { void *img; void *mem; void *view; uint32_t w, h; size_t alloc; uint32_t layout; uint32_t mips; uint32_t fmt, bpp; uint32_t storage, mtl_type, layers; int placed; uint32_t bw, bh; uint32_t samples; uint32_t sysmem; uint32_t vkflags, usage;  uint32_t vfl;  uint64_t up_token;
                 void *pool; size_t poff, poollen; /* NVMTL_IMAGE_POOL: the pool block this image lives in and its reserved range */
                 size_t hz_off, hz_len;   /* placed: the image's byte range of mem, for the hazard model (0 = all of mem) */
                 void *sparse;   /* a sparse image (NVMTL_SPARSE_TEXTURES=1): its nvmtl_vk_sparse_info, freed with the image */
                 uint8_t hz_ut; } nvk_image;   /* hazard model: 0 not yet known, 1 tracked, 2 untracked (Metal hazardTrackingMode) */
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
    uint64_t gmask2[2][4], bmask2[4]; uint32_t gmask2_ok, bmask2_ok;   /* the same with the framebuffer-fetch set (3) left out like the heap (NVMTL_DESC_MASK) */
    uint32_t ag_mask, ag_req; void *ag_src; size_t ag_srcn; void *ag_safe;
    uint64_t shape_identity;
    uint32_t lin;
    uint64_t tbm[2][4]; uint32_t tbany;
    uint32_t fetch_mask;   /* colour attachments the fragment shader reads ([[color(n)]], set 3 binding 192+n) */
    uint32_t fetch_zero;   /* framebuffer fetch is off but the fragment shader reads [[color(n)]]: set 3 holds the dummy image */
    uint32_t fetch_copy;   /* the colour attachments it reads through copies (fetch off, NVMTL_FETCH_COPY): nvmtl_fetch_copy.h */
    uint32_t stencil_emu;  /* the fragment shader's stencil export was lowered to a per-bit discard (push constant 160) */
    uint32_t skip_draws;   /* NVMTL_SKIP_FS names this fragment shader: its draws are not recorded (hang bisection) */
    void *gl_uniform_alt; /* optional owned GL-only pipeline; the original remains the draw-time fallback */
    uint32_t gl_uniform_layout, gl_uniform_slot[2], gl_uniform_range[2], gl_uniform_tracked;
    uint32_t db_dyn;   /* the depth bounds test and its range are dynamic state of this pipeline (depth attachment + depthBounds feature) */
    uint32_t view_mask;   /* multiview: the views this pipeline's render pass runs (vertex amplification), 0 for an ordinary pass */
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
                 uint64_t *icb_tabs[2]; unsigned icb_ntabs, icb_slot;   /* GPU-encoded ICB replay: per-slot address table copies (vertex, fragment) and the one in use, 1-based, 0 = none */
                 unsigned nobind, skipped;
                 void *dcache[32]; void *dcachepool; unsigned ndcache, dcache_gl_uniform;
                 unsigned in_rp, occlusion_reset; uint32_t wseq;
                 void *fetchview[NVMTL_NCOL];   /* the open pass's colour views, read by [[color(n)]] framebuffer fetch */
                 void *fcopyview[NVMTL_NCOL];   /* copies of the colour attachments, for fetch_copy pipelines */
                 void **fcp; unsigned nfcp, capfcp;   /* copy images this command buffer holds until it retires */
                 unsigned fetch_draw, topo;   /* the bound pipeline reads [[color(n)]] in this pass; last MTLPrimitiveType set */
                 /* the last framebuffer fetch set made in this command buffer and what it points at: a draw whose pass
                    and mask are the same reuses it (Core Animation draws hundreds of fetch draws per pass) */
                 void *fetch_last_set; uint32_t fetch_last_mask; void *fetch_last_view[NVMTL_NCOL];
                 /* stencil export emulation (nvmtl_semu_draw): the bound pipeline's layout when it exports stencil, and the
                    stencil reference / write masks / depth write last set, which each per-bit pass changes and restores */
                 void *semu_layout; uint32_t st_ref[2], st_wmask[2]; int depth_write;
                 int skip_group;   /* inside a debug group NVMTL_SKIP_GROUP names: draws are not recorded (hang bisection) */
                 unsigned direct_sass;   /* recorded a dispatch of compiled machine code: never reuse this pool */
                 void *occlusion_pages; uint32_t occlusion_count;
                 void *timing_pool; nvmtl_gpu_interval timing;
                 void *pt_pool; unsigned pt_n, pt_open; uint32_t pt_w[128], pt_h[128], pt_f[128]; uint32_t pt_fs[128][4];
                 /* NVMTL_DISPATCH_TIME: per-dispatch timestamps, allocated on first use */
                 void *dt_pool; unsigned dt_n; struct nvmtl_dt_rec *dt;
                 void *counter_recording; unsigned counter_error;
                 nvmtl_sample_pattern sample_pattern;
                 void **rvs; unsigned nrv, caprv;
                 unsigned restart_off;  /* GL turned primitive restart off (setPrimitiveRestartEnabled:NO) */
                 uint32_t dyn_out[2];   /* address-table dynamic offset of the set the last snapshot returned, per stage */
                 void *tab_out[2];      /* the table page buffer that set's descriptor names */
                 /* Vulkan graphics bindings persist until changed; compute has its own bind point. */
                 void *graphics_pipeline, *graphics_layout, *graphics_sets[3];
                 uint32_t graphics_set_count, graphics_offset_count, graphics_offsets[4], gl_uniform_dyn[2];
                 uint32_t gl_uniform_result; /* optional diagnostics for the next recorded draw */
                 /* hazard model inputs (nvmtl_hazard.h): writable buffer slots and storage textures per stage, the
                    sampled textures each stage's SPIR-V names; hz_dyn = the open pass began with vkCmdBeginRendering */
                 uint32_t hz_bufw[2]; uint64_t hz_texw[2][2]; uint64_t hz_tmask[2][4]; uint8_t hz_tmask_ok[2];
                 int hz_wm_dirty, hz_dyn, hz_force_full; uint32_t hz_att_skip;   /* untracked attachments: bit i colour, bit 8 depth */
                 const void *hzp_res; uint8_t hzp_kind, hzp_on, hzp_skip, hzp_bskip;   /* the resource of the binding being written */
                 uint8_t hz_draw_due;   /* something the next draw's hazard check must see arrived */
                 uint32_t hz_sn;   /* hazard model timing sample counter */
                 nvmtl_fx_cb fx;   /* fault context: the last dispatches and the storage binds in force (nvmtl_fault_context.h) */
                 uint64_t fx_seq;  /* the queue-submit sequence number this command buffer was submitted with */
                 uint32_t begin_id; /* this command buffer's begin ordinal: what its per-dispatch witness words carry */
                 /* the encoder's depth bounds (setDepthTestMinBound:maxBound:), re-emitted when a pipeline that has them as
                    dynamic state is bound, since binding a pipeline without them leaves that state undefined */
                 uint8_t db_on; float db_lo, db_hi;
                 int dead; /* made after the device was lost: records nothing, submits as "device removed" (nvmtl_vk_cmd_begin) */
               } nvk_cmdbuf;
int  nvmtl_vk_init(void);
int nvmtl_vk_sample_positions_backend(void);
int nvmtl_vk_sample_positions_default(uint32_t count, nvmtl_sample_pattern *out);
int  nvmtl_vk_cmd_abandon(nvk_cmdbuf *c);
void nvmtl_vk_cmd_counts(uint64_t *begun, uint64_t *submitted, uint64_t *abandoned);
const char *nvmtl_vk_device_name(void);
/* the GPU vendor (0x10de, 0x1002, 0x8086) of this process's Metal device; set from the accelerator it was made on */
uint16_t nvmtl_vk_set_accelerator(mach_port_t accelerator);
uint16_t nvmtl_vk_vendor(void);
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
                 uint64_t boxes; uint32_t box_stride; uint32_t is_boxes;
                 /* curve geometry (NVK_CURVES=1): a box geometry whose boxes the driver writes from the control points */
                 uint32_t is_curve; uint32_t curve_flags; uint32_t cp_stride; uint32_t radius_stride;
                 uint64_t cp_addr; uint64_t radius_addr; uint64_t curve_indices; } nvmtl_geom;
int  nvmtl_vk_rt_available(void);
int  nvmtl_vk_barycentrics_available(void);   // VK_KHR_fragment_shader_barycentric present and enabled-capable
int  nvmtl_vk_accel_sizes(const nvmtl_geom *g, uint32_t ngeom, int instance, uint32_t instance_count, size_t *as_size, size_t *scratch_size);
int  nvmtl_vk_accel_create(nvk_buffer *backing, size_t size, int instance, nvk_accel *out);
void nvmtl_vk_accel_destroy(nvk_accel *a);
int  nvmtl_vk_cmd_build_accel(nvk_cmdbuf *c, nvk_accel *dst, const nvmtl_geom *g, uint32_t ngeom, uint64_t instances_addr, uint32_t instance_count, uint64_t scratch_addr);
int  nvmtl_vk_cmd_copy_accel(nvk_cmdbuf *c, nvk_accel *src, nvk_accel *dst);
// The push constants of the GPU instance-conversion kernel. The structure table is four words per
// instanced structure: device address low, high, its intersection-function-table base, pad.
typedef struct nvmtl_convinst_args {
    uint64_t src, dst, uid, astab;
    uint32_t stride, count, mtl_type, n_as, matrix_layout;
} nvmtl_convinst_args;
// nvlog prints a repeated message 8 times and then once every 4096, so a count taken off the log is
// that cap rather than the number of events. These are the real counts for the ray-tracing path.
enum {
    NVMTL_RT_AS_VRAM, NVMTL_RT_AS_SYSMEM, NVMTL_RT_AS_FAILED,
    NVMTL_RT_BUILD_PRIM, NVMTL_RT_BUILD_INST, NVMTL_RT_NIL_STRUCT, NVMTL_RT_NIL_SCRATCH,
    NVMTL_RT_CONV_GPU, NVMTL_RT_CONV_HOST, NVMTL_RT_INSTANCES, NVMTL_RT_FMT_UNCARRIED,
    NVMTL_RT_AS_SHORT, NVMTL_RT_SCRATCH_SHORT, NVMTL_RT_SCRATCH_OURS,
    NVMTL_RT_AS_DEAD,
    NVMTL_RT_SIZE_UNKNOWN,
    NVMTL_RT_COUNTERS
};
extern unsigned long long nvmtl_rt_count[NVMTL_RT_COUNTERS];
void nvmtl_rt_count_bump(int which);
void nvmtl_rt_count_add(int which, unsigned long long n);
void nvmtl_rt_census(void);
int  nvmtl_vk_cmd_convert_instances(nvk_cmdbuf *c, const nvmtl_convinst_args *args);
int  nvmtl_vk_cmd_convert_witness(nvk_cmdbuf *c, nvk_buffer *converted, uint32_t count);
uint32_t nvmtl_vk_convert_witness_read(void *dst, uint32_t max_instances);
int  nvmtl_vk_convert_instances(const void *mtl_instances, uint32_t stride, uint32_t count, const uint64_t *as_addrs, uint32_t n_as, void *vk_instances_out,
                                uint32_t mtl_type,
                                uint32_t *uid_out,
                                const uint32_t *as_ift, uint32_t matrix_layout);
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
uint64_t nvmtl_vk_recommended_working_set(void);
uint64_t nvmtl_vk_vram_bytes(void);
void     nvmtl_vk_mem_split(uint64_t *vram, uint64_t *sys);
int  nvmtl_vk_image_create_ms(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp, uint32_t samples, nvk_image *out);
/* Sparse textures (NVMTL_SPARSE_TEXTURES=1). The image is made without memory; blocks are bound to a sparse heap's VkDeviceMemory. */
typedef struct { uint32_t gran[3], first_tail, mips, layers, w, h; uint64_t tail_size, tail_offset, tail_stride; int single_tail, strict; } nvmtl_vk_sparse_info;
typedef struct { uint32_t mip, layer, x, y, z, w, h, d; uint64_t mem_off; } nvmtl_vk_sparse_blk;
int  nvmtl_vk_sparse_textures_ok(void);
uint64_t nvmtl_vk_timestamp_frequency(void);
int  nvmtl_vk_image_create_sparse(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp, uint32_t mips, uint32_t layers, nvk_image *out);
/* binds (mem != NULL) or unbinds (mem == NULL) the blocks, and the mip tail when tail_size_to_bind != 0 (at tail_mem_off); waits for the bind */
int  nvmtl_vk_image_sparse_bind(nvk_image *img, void *mem, const nvmtl_vk_sparse_blk *blk, uint32_t n, uint64_t tail_mem_off, int bind_tail);
int  nvmtl_vk_ms_storage_ok(void);
int  nvmtl_vk_sparse_granularity(uint32_t vkfmt, int is3d, uint32_t samples, uint32_t out[3]);   /* the device's sparse block shape, 0 on success */
int  nvmtl_vk_cap_float32_filtering(void);   /* MTLDevice capability answers decided from the Vulkan bits (nvmtl_caps_query.h) */
int  nvmtl_vk_cap_float32_msaa(void);
int  nvmtl_vk_cap_pull_model(void);
int  nvmtl_vk_image_create_ms_storage(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp, uint32_t samples, nvk_image *out);
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
int  nvmtl_vk_have_provoking_last(void);
void nvmtl_vk_cmd_set_provoking_last(nvk_cmdbuf *c, int last);
void nvmtl_vk_cmd_set_blend_color(nvk_cmdbuf *c, const float rgba[4]);
void nvmtl_vk_cmd_set_stencil_ref(nvk_cmdbuf *c, uint32_t front, uint32_t back);
void nvmtl_vk_cmd_set_depth_bias(nvk_cmdbuf *c, float constant, float slope, float clamp);
int  nvmtl_vk_cmd_set_depth_bounds(nvk_cmdbuf *c, int on, float lo, float hi);
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
int  nvmtl_vk_bind_parked_sampler(nvk_cmdbuf *c, uint32_t set, uint32_t index, nvk_sampler *s);
void nvmtl_vk_parked_samplers_keep(nvk_cmdbuf *c, uint32_t set, uint32_t keep);
void nvmtl_vk_cmd_bind_set(nvk_cmdbuf *c, nvk_pipeline *p);
int  nvmtl_vk_compute_pipeline_create(const void *cs, size_t n, nvk_pipeline *out);
void nvmtl_vk_cmd_dispatch(nvk_cmdbuf *c, nvk_pipeline *p, uint32_t gx, uint32_t gy, uint32_t gz);
void nvmtl_vk_cmd_dispatch_threads(nvk_cmdbuf *c, nvk_pipeline *p, const uint32_t threads[3], const uint32_t tg[3]);
/* the kernel name the next dispatch on this thread is logged under (NVMTL_DISPATCH_TIME) */
void nvmtl_vk_dispatch_label(const char *name);
/* the kernel's SPIR-V hash (FNV-1a, the profile and pipeline-key hash) the next dispatch on this thread is recorded under */
void nvmtl_vk_dispatch_key(uint32_t spv_fnv, uint64_t air);
/* fault context: the label an image dies under (the texture's, set just before its image is destroyed) */
void nvmtl_vk_image_dying_label(const char *label);
/* hazard tracking between encoders (nvmtl_hazard.h, NVMTL_HAZARD) */
int  nvmtl_hz_mode(void);
void nvmtl_hz_shadow(int on);
int  nvmtl_hz_draw(nvk_cmdbuf *c);
int  nvmtl_hz_dispatch(nvk_cmdbuf *c, int first);
void nvmtl_hz_encoder_end(nvk_cmdbuf *c);
void nvmtl_hz_force_full(nvk_cmdbuf *c, int keep_nodep);
int  nvmtl_hz_nodep(nvk_cmdbuf *c);
int  nvmtl_hz_heaps_all(void);
void nvmtl_hz_declare_buffer(nvk_cmdbuf *c, const nvk_buffer *b, int w, int index);
void nvmtl_hz_declare_image(nvk_cmdbuf *c, const nvk_image *im, int w);
void nvmtl_hz_declare_any(nvk_cmdbuf *c, int w);
void nvmtl_hz_use_heap(nvk_cmdbuf *c, int tracked, void *mem, size_t size, int legacy);
int  nvmtl_hz_bind_texture(nvk_cmdbuf *c, const nvk_image *im, int untracked, uint32_t set, uint32_t index, void *view);
int  nvmtl_hz_bind_storage(nvk_cmdbuf *c, const nvk_image *im, int untracked, uint32_t set, uint32_t index, void *view);
int  nvmtl_hz_bind_texel(nvk_cmdbuf *c, const nvk_image *im, int untracked, uint32_t set, uint32_t index, void *view, int storage);
int  nvmtl_hz_bind_buffer(nvk_cmdbuf *c, int untracked, uint32_t set, uint32_t index, nvk_buffer *b, size_t offset);
void nvmtl_vk_hz_counts(uint64_t out[32]);
int  nvmtl_hz_dispatch_decides(void);   /* NVMTL_HAZARD_DISPATCH=1: later dispatches take their barrier from the model */
void nvmtl_vk_hz_reasons(double frames, char *out, size_t cap);
void nvmtl_vk_hz_reasons_log(double frames);
void nvmtl_vk_hz_cpu(uint64_t out_ns[8]);
int  nvmtl_hz_blit_begin(nvk_cmdbuf *c);
void nvmtl_hz_blit_key(nvk_cmdbuf *c, const nvk_buffer *b, const nvk_image *im, int w);
void nvmtl_hz_blit_go(nvk_cmdbuf *c);
void nvmtl_hz_blit_end(nvk_cmdbuf *c);
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
int nvmtl_vk_vram_measured(void);
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
int  nvmtl_vk_image_write_host(nvk_queue *q, nvk_image *img, void *base, size_t row_bytes, size_t bytes, nvk_buffer *cache);
int  nvmtl_vk_image_read_host(nvk_queue *q, nvk_image *img, void *base, size_t row_bytes, size_t bytes, nvk_buffer *cache);
int  nvmtl_vk_record_image_to_host(nvk_cmdbuf *c, nvk_image *img, const nvk_buffer *host, size_t offset, size_t row_bytes);
int  nvmtl_vk_host_pages_hold(void *base, size_t bytes, nvk_buffer *out);
void nvmtl_vk_host_pages_drop(nvk_buffer *cache);
/* The surface and cookie the next host-page copy on this thread belongs to (cookie 0 = none): lets wrappers of one surface share an import (NVMTL_HOST_PAGES_SHARE=1). */
void nvmtl_vk_host_pages_key(uint32_t surface_id, uint64_t cookie);
int  nvmtl_vk_copy_buffer(nvk_queue *q, nvk_buffer *src, size_t srcOff, nvk_buffer *dst, size_t dstOff, size_t size);
int  nvmtl_vk_image_create_mips(uint32_t w, uint32_t h, int bgra, uint32_t mips, nvk_image *out);
int  nvmtl_vk_image_create_ex(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp, int a8, uint32_t mips, nvk_image *out);
int  nvmtl_vk_image_create_ex2(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp, int a8, uint32_t mips, int storage, nvk_image *out);
int  nvmtl_vk_format_is_storage(uint32_t vkfmt);
int  nvmtl_vk_bind_storage_view(nvk_cmdbuf *c, uint32_t set, uint32_t index, void *view);
uint32_t nvmtl_vk_bindless_put(void *view);
void nvmtl_vk_bindless_put_storage(uint32_t slot, void *view);
uint32_t nvmtl_vk_bindless_put_texel(void *bufview, int storage);
uint32_t nvmtl_vk_bindless_sampler_slot(void *vksampler);
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
typedef struct { uint32_t ncol, cfmt[NVMTL_NCOL]; nvk_blend_state blend[NVMTL_NCOL]; uint32_t dfmt, discard, a2c, a2one; uint32_t view_mask; } nvk_rt;
typedef struct { nvk_image *col[NVMTL_NCOL]; void *colview[NVMTL_NCOL]; uint32_t colw[NVMTL_NCOL], colh[NVMTL_NCOL];
    nvk_image *res[NVMTL_NCOL]; void *resview[NVMTL_NCOL]; uint32_t resw[NVMTL_NCOL], resh[NVMTL_NCOL];
    float clear[NVMTL_NCOL][4]; nvk_image *ds; void *dsview; float clear_depth; uint32_t clear_stencil, load, w, h;
    uint32_t layers; nvk_image *dres; void *dresview; uint32_t dresmode, sresmode;
    nvmtl_sample_pattern sample_pattern; uint32_t depth_store_options, stencil_store_options;
    uint32_t ds_level, ds_layer;
    uint32_t colfmt[NVMTL_NCOL], resfmt[NVMTL_NCOL];
    uint32_t col_level[NVMTL_NCOL], col_layer[NVMTL_NCOL];  } nvk_pass;
typedef struct { uint32_t ncol, cfmts[NVMTL_NCOL], has_depth, dfmt, samples, view_mask; } nvk_rtsig;
/* the most views one multiview pass can run on this device; 0 when vertex amplification is off or the device cannot do it */
uint32_t nvmtl_vk_multiview_views(void);
int  nvmtl_vk_pipeline_create_rt(const void *vs, size_t vn, const void *fs, size_t fn, const nvk_rt *rt, const void *vin, uint32_t samples,
                                 const nvk_tess_desc *td, void *out);
void nvmtl_vk_pipeline_try_gl_uniform(const void *vs, size_t vn, const void *fs, size_t fn, uint32_t vi, uint32_t fi,
                                     const nvk_rt *rt, const void *vin, uint32_t samples, nvk_pipeline *base);
int  nvmtl_vk_cmd_begin_pass(nvk_cmdbuf *c, const nvk_pass *ps, nvk_pipeline *p);
int  nvmtl_vk_fetch_copy(nvk_cmdbuf *c, const nvk_pass *ps, uint32_t mask);   /* after the pass has ended: copy the colour attachments in mask */
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
/* empties the indirect-argument records (5 words each) outside the {location, length} range the GPU wrote at range+range_off */
int nvmtl_vk_cmd_y422_convert(nvk_cmdbuf *c, nvk_buffer *src, size_t src_off, size_t pitch, nvk_buffer *dst, uint32_t w, uint32_t h, uint32_t mtl_format);
int nvmtl_vk_cmd_icb_gather(nvk_cmdbuf *c, nvk_buffer *records, nvk_buffer *out, size_t out_off, uint32_t slots, uint32_t compute, uint64_t pso, uint32_t ptype,
                            uint32_t loc, uint32_t len, const uint32_t *tg, uint32_t indexed, uint32_t cap, nvk_buffer *range, size_t range_off);
int nvmtl_vk_cmd_icb_index_copy(nvk_cmdbuf *c, nvk_buffer *records, nvk_buffer *dst, uint32_t slots, uint32_t cap, uint32_t loc, uint32_t len, nvk_buffer *range, size_t range_off);
/* per-command buffers of a GPU-encoded ICB: one table copy per slot for each stage, patched on the GPU from the records; outside a render pass.
   dsts: scratch of 2*slots*8 bytes. nvmtl_vk_cmd_icb_use(slot) makes the next draws read slot's tables (0 = the ordinary tables). */
int  nvmtl_vk_cmd_icb_tables(nvk_cmdbuf *c, nvk_buffer *records, nvk_buffer *dsts, uint32_t slots);
void nvmtl_vk_cmd_icb_use(nvk_cmdbuf *c, uint32_t slot);
void nvmtl_vk_cmd_icb_tables_free(nvk_cmdbuf *c);
int nvmtl_vk_cmd_icb_mask(nvk_cmdbuf *c, nvk_buffer *args, nvk_buffer *range, size_t range_off, uint32_t slots, uint32_t zero);
void nvmtl_vk_cmd_draw_indexed_indirect(nvk_cmdbuf *c, nvk_buffer *idx, size_t idxOff, int is32, nvk_buffer *b, size_t off);
void nvmtl_vk_cmd_dispatch_indirect(nvk_cmdbuf *c, nvk_pipeline *p, nvk_buffer *b, size_t off);
void nvmtl_vk_cmd_barrier(nvk_cmdbuf *c);
void nvmtl_vk_cmd_barrier_c2c(nvk_cmdbuf *c);
/* Barrier census: which site asked for each full barrier. An encoder names its site before the call; a call from inside
   the Vulkan layer (copies, uploads, clears) counts as the layer's own. nvmtl_vk_bar_counts reads the totals. */
enum { NVMTL_BARS_LAYER, NVMTL_BARS_FIRST_DISPATCH, NVMTL_BARS_LATER_DISPATCH, NVMTL_BARS_ENCODER_END, NVMTL_BARS_OWED_FLUSH,
       NVMTL_BARS_FENCE_API, NVMTL_BARS_HAZARD_RESTART, NVMTL_BARS_OTHER, NVMTL_BARS_C2C, NVMTL_BARS_TRANSITION,
       NVMTL_BARS_TRANSITION_NOWAIT, NVMTL_BARS_TRANSITION_SKIPPED, NVMTL_BARS_PASS, NVMTL_BARS_N };
void nvmtl_vk_barrier_site(int site);
void nvmtl_vk_bar_counts(uint64_t out[16]);
// DLSS Super Resolution behind the MetalFX temporal scaler (nvmtl_dlss.h). available: the files are installed, the
// device has the NVX extensions, and the NGX core initialised. cmd_dlss records one upscale; *feature holds the
// scaler's NGX feature (NULL at first, made on the first call and again when a setting it was made for changes).
typedef struct {
    nvk_image *color, *depth, *motion, *output, *exposure, *reactive;   // exposure and reactive may be NULL
    void *color_view, *depth_view, *motion_view, *output_view, *exposure_view, *reactive_view;
    uint32_t in_w, in_h, out_w, out_h;   // the descriptor's sizes: what the feature is made for
    uint32_t cfg_in_w_used, cfg_in_h_used;   // the part of the input used this frame
    int hdr, mv_lowres, mv_jittered, depth_inverted, auto_exposure;
    float jitter_x, jitter_y, mv_scale_x, mv_scale_y, pre_exposure; int reset;
    uint32_t color_x, color_y, depth_x, depth_y, motion_x, motion_y;
} nvmtl_vk_dlss_args;
int nvmtl_vk_dlss_available(void);
int nvmtl_vk_cmd_dlss(nvk_cmdbuf *c, void **feature, const nvmtl_vk_dlss_args *a);
// DLSS Ray Reconstruction behind the MetalFX temporal denoised scaler. rr_available: nvmtl_vk_dlss_available and the core
// reported Ray Reconstruction. cmd_dlss_rr records one denoise + upscale; *feature works as for cmd_dlss.
typedef struct {
    nvk_image *color, *depth, *motion, *output, *diffuse_albedo, *specular_albedo, *normals, *roughness, *hit_distance, *exposure, *reactive;
    void *color_view, *depth_view, *motion_view, *output_view, *diffuse_albedo_view, *specular_albedo_view, *normals_view, *roughness_view,
         *hit_distance_view, *exposure_view, *reactive_view;
    uint32_t in_w, in_h, out_w, out_h;
    int hdr, mv_lowres, mv_jittered, depth_inverted, auto_exposure;
    float world_to_view[16], view_to_clip[16];
    float jitter_x, jitter_y, mv_scale_x, mv_scale_y, pre_exposure; int reset;
} nvmtl_vk_dlss_rr_args;
int nvmtl_vk_dlss_rr_available(void);
int nvmtl_vk_cmd_dlss_rr(nvk_cmdbuf *c, void **feature, const nvmtl_vk_dlss_rr_args *a);
// DLSS Frame Generation behind the MetalFX frame interpolator: one evaluation writes the in-between frame into output.
typedef struct {
    nvk_image *color, *depth, *motion, *output, *ui;   // ui may be NULL
    void *color_view, *depth_view, *motion_view, *output_view, *ui_view;
    uint32_t in_w, in_h, out_w, out_h;
    int hdr, depth_inverted, reset, camera_motion_included;
    float near_z, far_z, fov_y, aspect;
    float cam_pos[3], cam_right[3], cam_up[3], cam_fwd[3];
    float clip_to_prev[16], prev_to_clip[16], view_to_clip[16];
    float mv_scale_x, mv_scale_y, jitter_x, jitter_y;
    uint32_t frame_id, multi_index, multi_count;
} nvmtl_vk_dlss_fg_args;
int nvmtl_vk_dlss_fg_available(void);
int nvmtl_vk_cmd_dlss_fg(nvk_cmdbuf *c, void **feature, const nvmtl_vk_dlss_fg_args *a);
int nvmtl_vk_cmd_blit_convert(nvk_cmdbuf *c, nvk_image *src, uint32_t sl, uint32_t ss, uint32_t sx, uint32_t sy,
                              nvk_image *dst, uint32_t dl, uint32_t ds, uint32_t dx, uint32_t dy, uint32_t w, uint32_t h);
void nvmtl_vk_buffer_destroy(nvk_buffer *b);
int nvmtl_vk_buffer_readback(nvk_buffer *b, size_t off, size_t len, void *dst);
void nvmtl_vk_image_destroy(nvk_image *i);
// Finish any in-flight asynchronous texture upload to this image (NVMTL_ASYNC_TEX_UPLOAD) before its memory
// is read back, migrated or freed. A no-op when none is pending, which is always the case with the flag off.
void nvmtl_vk_image_upload_barrier(nvk_image *img);
void nvmtl_vk_queue_destroy(nvk_queue *q);
size_t nvmtl_vk_allocated_bytes(void);
uint64_t nvmtl_vk_buffer_address(nvk_buffer *b);

int nvmtl_vk_image_create_typed(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp, int a8, uint32_t mips, int storage, uint32_t mtl_type, uint32_t layers, nvk_image *out);
typedef struct { void *mem; void *map; size_t size; uint32_t memtype; int host_visible; } nvk_heap;
int  nvmtl_vk_heap_create(size_t size, int host_visible, nvk_heap *out);
void nvmtl_vk_image_parked(nvk_image *i);     /* the image went to a park for reuse (alive, recyclable) */
void nvmtl_vk_image_recycled(nvk_image *i);   /* a parked image was taken back into use */
int nvmtl_vk_heap_zero(nvk_heap *h, const char *what);   /* clear a fresh heap to zero before its first placement (NVMTL_HEAP_ZERO) */
int  nvmtl_vk_format_block(uint32_t vkfmt, uint32_t *bw, uint32_t *bh, uint32_t *block_bytes);
static inline uint32_t nvmtl_vk_layers_for_type(uint32_t t, uint32_t n) { n = n ? n : 1; return (t == 1 || t == 3 || t == 8) ? n : t == 5 ? 6u : t == 6 ? 6u * n : 1u; }
int nvmtl_vk_cmd_prepare_sampled_image(nvk_cmdbuf *c, nvk_image *img, int storage);
int  nvmtl_vk_image_view_create_range(nvk_image *img, uint32_t vkfmt, int a8, uint32_t baseLevel, uint32_t levelCount, uint32_t baseLayer, uint32_t layerCount, uint32_t mtl_view_type, void **out_view);
int  nvmtl_vk_image_view_create_range_aspect(nvk_image *img, uint32_t vkfmt, int a8, uint32_t aspect, uint32_t baseLevel, uint32_t levelCount, uint32_t baseLayer, uint32_t layerCount, uint32_t mtl_view_type, void **out_view);
int  nvmtl_vk_image_write_region_level_layer(nvk_queue *q, nvk_image *img, const void *src, size_t row_bytes, uint32_t x, uint32_t y, uint32_t w, uint32_t h, uint32_t level, uint32_t layer);
int  nvmtl_vk_image_read_region_level_layer(nvk_queue *q, nvk_image *img, void *dst, size_t row_bytes, uint32_t x, uint32_t y, uint32_t w, uint32_t h, uint32_t level, uint32_t layer);
int  nvmtl_vk_cmd_copy_image_sub(nvk_cmdbuf *c, nvk_image *src, uint32_t sl, uint32_t ss, uint32_t sx, uint32_t sy, nvk_image *dst, uint32_t dl, uint32_t ds, uint32_t dx, uint32_t dy, uint32_t w, uint32_t h);
/* Level 0, layer 0, colour: copies with format conversion (an sRGB side is decoded on read and encoded on write). */
int  nvmtl_vk_cmd_blit_image_2d(nvk_cmdbuf *c, nvk_image *src, nvk_image *dst, uint32_t w, uint32_t h);
/* 1 when the VkFormat is an 8-bit sRGB colour format. */
int  nvmtl_vk_srgb_format(uint32_t fmt);
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
int nvmtl_vk_surface_dirty_try(uint32_t surfaceID, uint32_t plane);
uint32_t nvmtl_vk_surface_gpu_mark(uint32_t surfaceID, int pending);
int nvmtl_vk_accel_surface_wait(void);
