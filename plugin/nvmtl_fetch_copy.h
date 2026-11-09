/* nvmtl_fetch_copy.h - framebuffer fetch emulated by copies: the SPIR-V side.
   With framebuffer fetch off (WindowServer's default) a fragment shader that reads its colour attachments ([[color(n)]],
   an input attachment at set 3, binding 192+n) reads a dummy image, and Core Animation's material composite (the Dock's
   and notifications' blur) draws flat tiles. Turning fetch on for every pass in WindowServer is what blanked the desktop,
   so only a pipeline that reads the attachments is changed: nvmtl_spirv_fetch_to_texture() rewrites its fragment module so
   each subpass-input read becomes a texel fetch of a sampled 2D image at the pixel's own position (OpImageFetch at
   FragCoord.xy, Lod 0), and the encoder copies the attachments to scratch images before such a draw (nvmtl_vk_fetch_copy).
   Returns a malloc'd module and its size in words, or NULL when it cannot (no subpass input, a multisampled one, more
   than one entry point, or a malformed module): the caller then leaves the shader as it was.
   Pure C so tools/test_fetch_copy.py can run it on modules and check them with spirv-val. The idea comes from the NvGfx
   project (watchthelight-labs/NvGfx); this implementation is our own. */
#ifndef NVMTL_FETCH_COPY_H
#define NVMTL_FETCH_COPY_H
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

enum { FC_OpCapability = 17, FC_OpEntryPoint = 15, FC_OpTypeInt = 21, FC_OpTypeFloat = 22, FC_OpTypeVector = 23,
       FC_OpTypeImage = 25, FC_OpTypePointer = 32, FC_OpConstant = 43, FC_OpFunction = 54, FC_OpVariable = 59,
       FC_OpLoad = 61, FC_OpDecorate = 71, FC_OpVectorShuffle = 79, FC_OpImageFetch = 95, FC_OpImageRead = 98,
       FC_OpConvertFToS = 110 };
enum { FC_DIM_2D = 1, FC_DIM_SUBPASS = 6, FC_DEC_BUILTIN = 11, FC_DEC_NONWRITABLE = 24, FC_DEC_INPUT_ATTACHMENT_INDEX = 43,
       FC_BUILTIN_FRAGCOORD = 15, FC_CAP_INPUT_ATTACHMENT = 40, FC_SC_INPUT = 1, FC_IMG_LOD = 2 };

typedef struct { uint32_t *w; size_t n, cap; int oom; } fc_out;
static inline void fc_put(fc_out *o, const uint32_t *src, size_t n)
{
    if (o->oom) return;
    if (o->n + n > o->cap) {
        size_t cap = o->cap ? o->cap : 1024;
        while (cap < o->n + n) cap *= 2;
        uint32_t *g = (uint32_t *)realloc(o->w, cap * 4);
        if (!g) { o->oom = 1; return; }
        o->w = g; o->cap = cap;
    }
    memcpy(o->w + o->n, src, n * 4);
    o->n += n;
}
/* one instruction with up to 6 operands */
static inline void fc_ins(fc_out *o, uint32_t op, uint32_t a, uint32_t b, uint32_t c, uint32_t d, uint32_t e, uint32_t f, int nops)
{
    uint32_t v[7] = { ((uint32_t)(nops + 1) << 16) | op, a, b, c, d, e, f };
    fc_put(o, v, (size_t)nops + 1);
}

static inline uint32_t *nvmtl_spirv_fetch_to_texture(const uint32_t *in, size_t nw, size_t *outw)
{
    *outw = 0;
    if (nw < 5 || in[0] != 0x07230203u) return NULL;
    uint32_t bound = in[3];
    if (bound == 0 || bound > (1u << 22)) return NULL;
    uint32_t *kind = (uint32_t *)calloc(bound, sizeof(uint32_t));     /* 1 subpass image type, 2 pointer to one, 3 its variable, 4 a loaded image */
    uint32_t *twin = (uint32_t *)calloc(bound, sizeof(uint32_t));     /* subpass image type -> an equal sampled 2D type already declared */
    if (!kind || !twin) { free(kind); free(twin); return NULL; }
    uint32_t t_float = 0, t_v2f = 0, t_v4f = 0, t_int = 0, t_v2i = 0, t_ptr = 0, v_fc = 0;
    size_t first_fn = 0, last_annot = 0, first_type = 0; int entries = 0, subs = 0;
    uint32_t sub_ids[16];
    size_t twin_at[16] = { 0 };   /* where each subpass type's twin is declared */
    /* pass 1: scalar and image types, annotations, the function boundary */
    for (size_t i = 5; i < nw;) {
        uint32_t wc = in[i] >> 16, op = in[i] & 0xffffu;
        if (!wc || i + wc > nw) goto fail;
        if (op == FC_OpEntryPoint) entries++;
        if (op == FC_OpTypeFloat && wc >= 3 && in[i + 2] == 32 && !t_float) t_float = in[i + 1];
        if (op == FC_OpTypeInt && wc >= 4 && in[i + 2] == 32 && in[i + 3] == 1 && !t_int) t_int = in[i + 1];
        if (op == FC_OpTypeImage && wc >= 9 && in[i + 3] == FC_DIM_SUBPASS) {
            if (in[i + 6] != 0 || subs >= 16 || in[i + 1] >= bound) goto fail;   /* multisampled subpass input: not emulated */
            sub_ids[subs++] = in[i + 1];
            kind[in[i + 1]] = 1;
        }
        if (op == FC_OpDecorate && wc >= 4 && in[i + 2] == FC_DEC_BUILTIN && in[i + 3] == FC_BUILTIN_FRAGCOORD && in[i + 1] < bound) v_fc = in[i + 1];
        if (op == FC_OpFunction && !first_fn) first_fn = i;
        if ((op >= 71 && op <= 75) || op == 332 || op == 5632 || op == 5633) last_annot = i + wc;
        if (!first_type && op >= 19 && op <= 39) first_type = i;
        i += wc;
    }
    if (!subs || entries != 1 || !first_fn || !t_float || !first_type) goto fail;
    /* pass 2: vectors, then the Input pointer to float4, and each subpass type's sampled 2D twin */
    for (size_t i = 5; i < nw;) {
        uint32_t wc = in[i] >> 16, op = in[i] & 0xffffu;
        if (op == FC_OpTypeVector && wc >= 4) {
            if (in[i + 2] == t_float && in[i + 3] == 2 && !t_v2f) t_v2f = in[i + 1];
            if (in[i + 2] == t_float && in[i + 3] == 4 && !t_v4f) t_v4f = in[i + 1];
            if (t_int && in[i + 2] == t_int && in[i + 3] == 2 && !t_v2i) t_v2i = in[i + 1];
        }
        i += wc;
    }
    for (size_t i = 5; t_v4f && i < nw;) {
        uint32_t wc = in[i] >> 16, op = in[i] & 0xffffu;
        if (op == FC_OpTypePointer && wc >= 4 && in[i + 2] == FC_SC_INPUT && in[i + 3] == t_v4f) { t_ptr = in[i + 1]; break; }
        i += wc;
    }
    for (int k = 0; k < subs; k++) {
        uint32_t sampled = 0;
        for (size_t i = 5; i < nw;) {
            uint32_t wc = in[i] >> 16;
            if ((in[i] & 0xffffu) == FC_OpTypeImage && in[i + 1] == sub_ids[k]) { sampled = in[i + 2]; break; }
            i += wc;
        }
        for (size_t i = 5; i < nw && sampled;) {
            uint32_t wc = in[i] >> 16;
            if ((in[i] & 0xffffu) == FC_OpTypeImage && wc == 9 && in[i + 2] == sampled && in[i + 3] == FC_DIM_2D && in[i + 4] == 0 &&
                in[i + 5] == 0 && in[i + 6] == 0 && in[i + 7] == 1 && in[i + 8] == 0) { twin[sub_ids[k]] = in[i + 1]; twin_at[k] = i; break; }
            i += wc;
        }
    }
    /* pass 3: pointers, variables and loads of subpass images (declaration order guarantees each is seen after its type) */
    for (size_t i = 5; i < nw;) {
        uint32_t wc = in[i] >> 16, op = in[i] & 0xffffu;
        if (op == FC_OpTypePointer && wc >= 4 && in[i + 3] < bound && kind[in[i + 3]] == 1 && in[i + 1] < bound) kind[in[i + 1]] = 2;
        if (op == FC_OpVariable && wc >= 4 && in[i + 1] < bound && kind[in[i + 1]] == 2 && in[i + 2] < bound) kind[in[i + 2]] = 3;
        if (op == FC_OpLoad && wc >= 4 && in[i + 1] < bound && kind[in[i + 1]] == 1 && in[i + 2] < bound) kind[in[i + 2]] = 4;
        i += wc;
    }
    {
        /* new ids: only what the module does not already have */
        const uint32_t id_int = t_int ? t_int : bound++;
        const uint32_t id_v2f = t_v2f ? t_v2f : bound++;
        const uint32_t id_v4f = t_v4f ? t_v4f : bound++;
        const uint32_t id_v2i = t_v2i ? t_v2i : bound++;
        const uint32_t id_ptr = t_ptr ? t_ptr : bound++;
        const uint32_t id_fc = v_fc ? v_fc : bound++;
        const uint32_t id_zero = bound++;
        fc_out o = { 0 };
        fc_put(&o, in, 5);
        for (size_t i = 5; i < nw;) {
            uint32_t wc = in[i] >> 16, op = in[i] & 0xffffu;
            if (i == first_fn) {   /* globals end here: new types, the FragCoord variable and the zero constant */
                if (!t_int) fc_ins(&o, FC_OpTypeInt, id_int, 32, 1, 0, 0, 0, 3);
                if (!t_v2f) fc_ins(&o, FC_OpTypeVector, id_v2f, t_float, 2, 0, 0, 0, 3);
                if (!t_v4f) fc_ins(&o, FC_OpTypeVector, id_v4f, t_float, 4, 0, 0, 0, 3);
                if (!t_v2i) fc_ins(&o, FC_OpTypeVector, id_v2i, id_int, 2, 0, 0, 0, 3);
                if (!t_ptr) fc_ins(&o, FC_OpTypePointer, id_ptr, FC_SC_INPUT, id_v4f, 0, 0, 0, 3);
                if (!v_fc) fc_ins(&o, FC_OpVariable, id_ptr, id_fc, FC_SC_INPUT, 0, 0, 0, 3);
                fc_ins(&o, FC_OpConstant, id_int, id_zero, 0, 0, 0, 0, 3);
            }
            if (!v_fc && i == (last_annot ? last_annot : first_type))   /* right after the annotations, or before the types when there are none */
                fc_ins(&o, FC_OpDecorate, id_fc, FC_DEC_BUILTIN, FC_BUILTIN_FRAGCOORD, 0, 0, 0, 3);
            { int moved = 0;   /* a twin declared after its subpass type was emitted at the subpass type's place */
              for (int k = 0; k < subs; k++) if (twin_at[k] == i && twin_at[k] > 0) {
                  size_t sp = 0;
                  for (size_t j = 5; j < i;) { uint32_t w2 = in[j] >> 16; if ((in[j] & 0xffffu) == FC_OpTypeImage && in[j + 1] == sub_ids[k]) { sp = j; break; } j += w2; }
                  if (sp) moved = 1;
              }
              if (moved) { i += wc; continue; } }
            if (op == FC_OpCapability && wc == 2 && in[i + 1] == FC_CAP_INPUT_ATTACHMENT) { i += wc; continue; }
            if (op == FC_OpDecorate && wc >= 4 && in[i + 2] == FC_DEC_INPUT_ATTACHMENT_INDEX) { i += wc; continue; }
            if (op == FC_OpDecorate && wc == 3 && in[i + 1] < bound && kind[in[i + 1]] == 3 && in[i + 2] == FC_DEC_NONWRITABLE) { i += wc; continue; }
            if (op == FC_OpTypeImage && wc >= 9 && in[i + 3] == FC_DIM_SUBPASS) {
                if (twin[in[i + 1]]) {   /* an equal sampled 2D type exists: it takes this place, so no use precedes its declaration */
                    for (int k = 0; k < subs; k++) if (sub_ids[k] == in[i + 1] && twin_at[k] > i) {
                        uint32_t twc = in[twin_at[k]] >> 16;
                        fc_put(&o, in + twin_at[k], twc);
                    }
                    i += wc; continue;
                }
                uint32_t b[16]; memcpy(b, in + i, (size_t)(wc < 16 ? wc : 16) * 4);
                b[3] = FC_DIM_2D; b[7] = 1;                         /* a sampled 2D image, read with OpImageFetch */
                fc_put(&o, b, wc); i += wc; continue;
            }
            if (op == FC_OpTypePointer && wc >= 4 && in[i + 3] < bound && twin[in[i + 3]]) {
                uint32_t b[4] = { in[i], in[i + 1], in[i + 2], twin[in[i + 3]] }; fc_put(&o, b, 4); i += wc; continue; }
            if (op == FC_OpLoad && wc >= 4 && in[i + 1] < bound && twin[in[i + 1]]) {
                uint32_t b[16]; memcpy(b, in + i, (size_t)(wc < 16 ? wc : 16) * 4); b[1] = twin[in[i + 1]]; fc_put(&o, b, wc); i += wc; continue; }
            if (op == FC_OpEntryPoint && !v_fc) {   /* the new FragCoord joins the interface list */
                uint32_t hdr = ((wc + 1) << 16) | op;
                fc_put(&o, &hdr, 1); fc_put(&o, in + i + 1, wc - 1); fc_put(&o, &id_fc, 1);
                i += wc; continue;
            }
            if (op == FC_OpImageRead && wc >= 5 && in[i + 3] < bound && kind[in[i + 3]] == 4) {
                const uint32_t a = bound++, s = bound++, c = bound++;
                fc_ins(&o, FC_OpLoad, id_v4f, a, id_fc, 0, 0, 0, 3);
                fc_ins(&o, FC_OpVectorShuffle, id_v2f, s, a, a, 0, 1, 6);
                fc_ins(&o, FC_OpConvertFToS, id_v2i, c, s, 0, 0, 0, 3);
                fc_ins(&o, FC_OpImageFetch, in[i + 1], in[i + 2], in[i + 3], c, FC_IMG_LOD, id_zero, 6);
                i += wc; continue;
            }
            fc_put(&o, in + i, wc);
            i += wc;
        }
        free(kind); free(twin);
        if (o.oom) { free(o.w); return NULL; }
        o.w[3] = bound;
        *outw = o.n;
        return o.w;
    }
fail:
    free(kind); free(twin);
    return NULL;
}
#endif
