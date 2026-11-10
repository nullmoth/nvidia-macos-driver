/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

/* DLSS Super Resolution through NVIDIA's NGX core. See nvmtl_dlss.h.
 *
 * NGX public types and parameter names follow NVIDIA's published DLSS SDK documentation; they are declared here
 * rather than copied. The NGX core also asks the NVIDIA management library about the GPU, so a small stand-in answers
 * those calls from the Vulkan device (see the chip table). */
#include "nvmtl_dlss.h"
#include "dlss/elfhost.h"
#include <dirent.h>
#include <fenv.h>
#include <locale.h>
#include <signal.h>
#include <xlocale.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <errno.h>
#include <unistd.h>
#include <fcntl.h>
#include <wchar.h>

void nvlog(const char *fmt, ...);
void nvlog_every(const char *fmt, ...);
void nvmtl_heaps_dump(FILE *f, void (*emit)(void *, const char *));   /* NVMTLObjects.m */
void nvmtl_heap_lazy_census(const char *why);                          /* NVMTLObjects.m */
static void dlss_heap_line(void *c, const char *l) { (void)c; nvlog_every("dlss: at FG out-of-memory, %s", l); }

/* The core and the runtime it loads execute on the calling thread. Their C library shims may reset the floating-point
 * environment (fesetenv), change the thread's locale, or change the signal mask; a caller that tracks those (a
 * translated game keeps its rounding and flush modes in them) must see them unchanged. */
typedef struct { fenv_t env; locale_t loc; sigset_t mask; } thread_env;
static void env_save(thread_env *t) { fegetenv(&t->env); t->loc = uselocale((locale_t)0); pthread_sigmask(SIG_SETMASK, NULL, &t->mask); }
static void env_restore(const thread_env *t) { fesetenv(&t->env); uselocale(t->loc); pthread_sigmask(SIG_SETMASK, &t->mask, NULL); }

/* ---- which device is this: PCI device id -> management-library architecture and chip code ------------------ */
/* The core asks the management library for each GPU's architecture (the public NVML enum: Turing 6, Ampere 7, Ada 8,
 * Blackwell 10) and for one more per-GPU value through the library's private export table, slot 257 (the "chip code").
 * The core's own architecture ids are the public ones in the open kernel modules (src/common/inc/swref/published/
 * nv_arch.h): Turing 0x160, Ampere 0x170, Ada 0x190, Blackwell GB1xx (data centre) 0x1A0, Blackwell GB2xx (GeForce RTX 50)
 * 0x1B0. Measured, by logging the core's decision: on a GeForce RTX 5060 (device id 0x2d05) with NVML architecture 10 and
 * chip code 0x60 the core selects 0x1B0. What slot 257 returns for other GPUs is not documented anywhere public, so the
 * code is only verified for that one part; the same constant is used for every GB20x row until another part is measured.
 * The Turing, Ampere and Ada rows use the same constant and are unmeasured: no such card has been run. */
typedef struct { uint16_t lo, hi; unsigned arch; int chip; const char *name; } chip_row;
static const chip_row k_chips[] = {
    { 0x2900, 0x2fff, 10, 0x60, "Blackwell (GB20x)" },   /* chip 0x60 measured on 0x2d05; maps to 0x1b0 */
    { 0x2600, 0x28ff,  8, 0x60, "Ada (AD10x)" },         /* arch 8 maps to 0x190 */
    { 0x2200, 0x25ff,  7, 0x60, "Ampere (GA10x)" },      /* arch 7 maps to 0x170 */
    { 0x2000, 0x20ff,  7, 0x60, "Ampere (GA100)" },      /* same */
    { 0x2180, 0x21ff,  6, 0x60, "Turing (TU11x)" },      /* arch 6 maps to 0x160 */
    { 0x1e00, 0x1fff,  6, 0x60, "Turing (TU10x)" },      /* same */
};
static const chip_row *chip_for(uint32_t device)
{
    for (size_t i = 0; i < sizeof k_chips / sizeof k_chips[0]; i++)
        if (device >= k_chips[i].lo && device <= k_chips[i].hi) return &k_chips[i];
    return NULL;
}

/* ---- state ---------------------------------------------------------------------------------------------- */
static pthread_mutex_t g_lock = PTHREAD_MUTEX_INITIALIZER;
static int g_state;   /* 0 untried, 1 ready, -1 failed for good */
static VkInstance g_inst; static VkDevice g_dev;
static PFN_vkGetInstanceProcAddr g_gipa; static PFN_vkGetDeviceProcAddr g_gdpa;
static uint32_t g_vendor, g_device; static const chip_row *g_chip;
static elfhost_module *g_core;
static int g_rr_avail, g_fg_avail;   /* the core's answers for Ray Reconstruction and Frame Generation (1 = yes) */

typedef struct { void **vt; } ngxp;   /* parameter object; vtable: 0 SetULL 1 SetF 3 SetUI 4 SetI 7 SetVoidPointer 12 GetI */
#define P_SETF(p, n, v)  ((void (*)(ngxp *, const char *, float))(p)->vt[1])((p), (n), (v))
#define P_SETUI(p, n, v) ((void (*)(ngxp *, const char *, unsigned))(p)->vt[3])((p), (n), (v))
#define P_SETI(p, n, v)  ((void (*)(ngxp *, const char *, int))(p)->vt[4])((p), (n), (v))
#define P_SETP(p, n, v)  ((void (*)(ngxp *, const char *, void *))(p)->vt[7])((p), (n), (v))
#define P_GETI(p, n, o)  ((unsigned (*)(ngxp *, const char *, int *))(p)->vt[12])((p), (n), (o))
typedef unsigned (*alloc_fn)(ngxp **);
typedef unsigned (*create_fn)(void *dev, void *cmd, int node, ngxp *params, void **handle);
typedef unsigned (*eval_fn)(void *cmd, void *handle, ngxp *params, void *cb);
static alloc_fn g_alloc; static create_fn g_create; static eval_fn g_eval;

/* NVSDK_NGX_Resource_VK: view, image, subresource range, format, size, then type (0 = image view) and read-write. */
typedef struct { VkImageView view; VkImage image; VkImageSubresourceRange range; VkFormat format; unsigned w, h; } ngx_image_vk;
typedef struct { ngx_image_vk image; int type; bool rw; } ngx_res_vk;

struct nvmtl_dlss_feature { void *handle; ngxp *eval; nvmtl_dlss_cfg cfg; };
struct nvmtl_dlss_fg_feature { void *handle; ngxp *eval; nvmtl_dlss_fg_cfg cfg; float clip_to_prev[16], prev_to_clip[16], view_to_clip[16]; };
struct nvmtl_dlss_rr_feature { void *handle; ngxp *eval; nvmtl_dlss_rr_cfg cfg; float world_to_view[16], view_to_clip[16]; };

/* ---- the files ---------------------------------------------------------------------------------------------- */
static int is_dir(const char *p) { struct stat s; return stat(p, &s) == 0 && S_ISDIR(s.st_mode); }
/* The core's file name carries the driver version, so find it instead of naming one. The newest name wins. */
static int find_core(char *out, size_t n)
{
    DIR *d = opendir(NVMTL_DLSS_NGX_DIR); if (!d) return -1;
    char best[256] = ""; struct dirent *e;
    while ((e = readdir(d)))
        if (!strncmp(e->d_name, "libnvidia-ngx.so.", 17) && strlen(e->d_name) < sizeof best && strcmp(e->d_name, best) > 0) strcpy(best, e->d_name);
    closedir(d);
    if (!best[0]) return -1;
    snprintf(out, n, "%s/%s", NVMTL_DLSS_NGX_DIR, best); return 0;
}
int nvmtl_dlss_wanted(void)
{
    static int cached = -1;
    if (cached < 0) {
        const char *e = getenv("NVMTL_DLSS"); char core[512];
        if (e && !strcmp(e, "0")) { cached = 0; nvlog("dlss: off (NVMTL_DLSS=0)"); }
        /* The desktop compositor never upscales; keep its Vulkan device exactly as before (no NVX extensions, no extra
         * features), so the DLSS files being installed can never change what WindowServer runs. */
        else if (!strcmp(getprogname(), "WindowServer")) cached = 0;
        else if (!is_dir(NVMTL_DLSS_SNIPPET_DIR) || find_core(core, sizeof core)) {
            cached = 0;
            if (e && !strcmp(e, "1")) nvlog("dlss: NVMTL_DLSS=1 but the files are not installed under %s", NVMTL_DLSS_ROOT);
        } else { cached = 1; nvlog("dlss: files found under %s", NVMTL_DLSS_ROOT); }
    }
    return cached;
}

/* ---- management-library stand-in -------------------------------------------------------------------------- */
static void *const NVML_H = (void *)0x4e564d4cULL, *const VK_H = (void *)0x56554c4bULL;
static int nv_ok(void) { return 0; }
static int nv_count(unsigned *n) { *n = 1; return 0; }
static int nv_byidx(unsigned i, void **d) { static char dev; if (i) return 2; *d = &dev; return 0; }
static int nv_arch(void *d, unsigned *a) { *a = g_chip ? g_chip->arch : 0; return 0; }
static int etbl_chip(void *d, int *c) { *c = g_chip ? g_chip->chip : 0; return 0; }
static int etbl_none(void) { return 3; }
static void *g_etbl[320];
static int nv_etbl(const void **t, const void *uuid)
{
    for (int i = 0; i < 320; i++) g_etbl[i] = (void *)etbl_none;
    g_etbl[257] = (void *)etbl_chip; *t = g_etbl; return 0;
}
static int nv_drv(char *b, unsigned n) { snprintf(b, n, "620.70.06"); return 0; }
static int nv_name(void *d, char *b, unsigned n) { snprintf(b, n, "NVIDIA GPU %04x", g_device); return 0; }
static int nv_pci(void *d, uint8_t *p) { memset(p, 0, 68); ((uint32_t *)(p + 16))[3] = (g_device << 16) | g_vendor; return 0; }
static int nv_unsupported(void) { return 3; }
static int nv_uuid(void *d, char *b, unsigned n) { snprintf(b, n, "GPU-%04x%04x-0000-0000-0000-000000000000", g_device, g_vendor); return 0; }
static int nv_cc(void *d, int *maj, int *min) { *maj = 12; *min = 0; return 0; }
static const char *nv_err(int e) { return "nvml stand-in"; }
static const struct { const char *n; void *f; } k_nvml[] = {
    {"nvmlInitWithFlags", nv_ok}, {"nvmlInit_v2", nv_ok}, {"nvmlShutdown", nv_ok}, {"nvmlDeviceGetCount_v2", nv_count},
    {"nvmlDeviceGetHandleByIndex_v2", nv_byidx}, {"nvmlDeviceGetArchitecture", nv_arch}, {"nvmlInternalGetExportTable", nv_etbl},
    {"nvmlSystemGetDriverVersion", nv_drv}, {"nvmlDeviceGetName", nv_name}, {"nvmlDeviceGetPciInfo_v3", nv_pci},
    {"nvmlInit", nv_ok}, {"nvmlDeviceGetCount", nv_count}, {"nvmlDeviceGetHandleByIndex", nv_byidx},
    {"nvmlDeviceGetUUID", nv_uuid}, {"nvmlDeviceGetCudaComputeCapability", nv_cc}, {"nvmlErrorString", nv_err},
    {"nvmlDeviceGetPciInfo", nv_pci}, {"nvmlDeviceGetPciInfo_v2", nv_pci},
};

/* ---- address witness (NVMTL_DLSS_TRACE=1) ------------------------------------------------------------------------
 * A Cyberpunk run faulted the GPU channel on a compute-shader access at VA 0x0 once DLSS was serving the scaler. These
 * wrappers sit between the core and the Vulkan driver and log every address or handle the core is given that comes
 * back 0, the name of each compiled function, and the first launches of each function with their 8-byte parameter
 * words, so the run names which kernel received the null and where it came from. Off unless the switch is set. */
static FILE *g_trf; static pthread_mutex_t g_trf_mu = PTHREAD_MUTEX_INITIALIZER;
/* The trace goes to its own file ($TMPDIR/dlss-trace.txt): nvlog keeps only the first 8 lines of each message format,
 * which cut the first trace off after 8 launches. */
static void tr(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
static void tr(const char *fmt, ...)
{
    pthread_mutex_lock(&g_trf_mu);
    if (!g_trf) {
        char path[600]; const char *t = getenv("TMPDIR");
        snprintf(path, sizeof path, "%s/dlss-trace.txt", t && *t ? t : "/tmp");
        /* never through a link: without TMPDIR this is a fixed name in a folder every user can write */
        { const int fd = open(path, O_WRONLY | O_CREAT | O_APPEND | O_NOFOLLOW | O_CLOEXEC, 0600); g_trf = fd < 0 ? NULL : fdopen(fd, "a"); if (fd >= 0 && !g_trf) close(fd); }
        if (g_trf) { setvbuf(g_trf, NULL, _IOLBF, 0); nvlog("dlss: trace file %s", path); }
    }
    if (g_trf) { va_list ap; va_start(ap, fmt); vfprintf(g_trf, fmt, ap); va_end(ap); fputc('\n', g_trf); }
    pthread_mutex_unlock(&g_trf_mu);
}
static int trace_on(void) { static int on = -1; if (on < 0) { const char *e = getenv("NVMTL_DLSS_TRACE"); on = e && e[0] == '1'; } return on; }
static PFN_vkGetImageViewHandle64NVX r_ivh64;
static PFN_vkGetBufferDeviceAddress r_bda; static PFN_vkGetImageViewHandleNVX r_ivh; static PFN_vkGetImageViewAddressNVX r_iva;
static PFN_vkCreateCuFunctionNVX r_cfn; static PFN_vkCmdCuLaunchKernelNVX r_launch;
#define TR_FN 256
static struct { VkCuFunctionNVX fn; char name[96]; unsigned launches; } g_tr[TR_FN]; static unsigned g_trn;
static pthread_mutex_t g_tr_mu = PTHREAD_MUTEX_INITIALIZER;
static VkDeviceAddress VKAPI_CALL w_bda(VkDevice d, const VkBufferDeviceAddressInfo *i)
{ VkDeviceAddress a = r_bda(d, i); if (!a) tr("vkGetBufferDeviceAddress(buffer %p) = 0", (void *)i->buffer); return a; }
static uint32_t VKAPI_CALL w_ivh(VkDevice d, const VkImageViewHandleInfoNVX *i)
{ uint32_t h = r_ivh(d, i); tr("image view handle view %p type %d sampler %p = 0x%x", (void *)i->imageView, i->descriptorType, (void *)i->sampler, h); return h; }
static uint64_t VKAPI_CALL w_ivh64(VkDevice d, const VkImageViewHandleInfoNVX *i)
{ uint64_t h = r_ivh64(d, i); tr("image view handle64 view %p type %d sampler %p = 0x%llx", (void *)i->imageView, i->descriptorType, (void *)i->sampler, (unsigned long long)h); return h; }
static VkResult VKAPI_CALL w_iva(VkDevice d, VkImageView v, VkImageViewAddressPropertiesNVX *p)
{ VkResult r = r_iva(d, v, p); tr("image view address view %p -> %d addr 0x%llx size %llu", (void *)v, r, (unsigned long long)p->deviceAddress, (unsigned long long)p->size); return r; }
static VkResult VKAPI_CALL w_cfn(VkDevice d, const VkCuFunctionCreateInfoNVX *ci, const VkAllocationCallbacks *a, VkCuFunctionNVX *f)
{
    VkResult r = r_cfn(d, ci, a, f);
    pthread_mutex_lock(&g_tr_mu);
    if (r == VK_SUCCESS && g_trn < TR_FN) { g_tr[g_trn].fn = *f; snprintf(g_tr[g_trn].name, sizeof g_tr[g_trn].name, "%s", ci->pName ? ci->pName : "?"); g_trn++; }
    pthread_mutex_unlock(&g_tr_mu);
    return r;
}
/* NVMTL_DLSS_SKIP (bisection, with the trace on): "all" records no NVX launch, "vk" no Vulkan dispatch of the runtime,
 * otherwise a comma-separated list of kernel-name substrings whose launches are not recorded. The frame is wrong, but
 * a GPU fault that survives a skip did not come from the skipped work. */
static int skip_match(const char *name)
{
    const char *e = getenv("NVMTL_DLSS_SKIP");
    if (!e || !*e) return 0;
    if (!strcmp(e, "all")) return 1;
    char buf[512]; snprintf(buf, sizeof buf, "%s", e);
    for (char *t = strtok(buf, ","); t; t = strtok(NULL, ",")) if (*t && strcmp(t, "vk") && strstr(name, t)) return 1;
    return 0;
}
static int skip_vk(void) { const char *e = getenv("NVMTL_DLSS_SKIP"); return e && (strstr(e, "vk") != NULL); }
static PFN_vkCmdDispatch r_disp; static PFN_vkCmdPushDescriptorSetKHR r_pds; static PFN_vkCmdBindPipeline r_bp;
static unsigned g_ndisp, g_npds;
static void VKAPI_CALL w_disp(VkCommandBuffer cb, uint32_t x, uint32_t y, uint32_t z)
{
    const unsigned n = ++g_ndisp;
    if (n <= 64) tr("vk dispatch #%u %u,%u,%u%s", n, x, y, z, skip_vk() ? " SKIPPED" : "");
    if (!skip_vk()) r_disp(cb, x, y, z);
}
static void VKAPI_CALL w_pds(VkCommandBuffer cb, VkPipelineBindPoint bp, VkPipelineLayout l, uint32_t set, uint32_t n, const VkWriteDescriptorSet *w)
{
    if (++g_npds <= 64)
        for (uint32_t k = 0; k < n; k++) {
            const VkWriteDescriptorSet *d = &w[k];
            if (d->pImageInfo) tr("vk push set %u binding %u type %d view %p sampler %p", set, d->dstBinding, d->descriptorType,
                                  (void *)d->pImageInfo[0].imageView, (void *)d->pImageInfo[0].sampler);
            else if (d->pBufferInfo) tr("vk push set %u binding %u type %d buffer %p off %llu range %llu", set, d->dstBinding, d->descriptorType,
                                        (void *)d->pBufferInfo[0].buffer, (unsigned long long)d->pBufferInfo[0].offset, (unsigned long long)d->pBufferInfo[0].range);
            else tr("vk push set %u binding %u type %d (other)", set, d->dstBinding, d->descriptorType);
        }
    r_pds(cb, bp, l, set, n, w);
}
static void VKAPI_CALL w_bp(VkCommandBuffer cb, VkPipelineBindPoint bp, VkPipeline p) { tr("vk bind pipeline %d %p", bp, (void *)p); r_bp(cb, bp, p); }
static void VKAPI_CALL w_launch(VkCommandBuffer cb, const VkCuLaunchInfoNVX *li)
{
    const char *name = "?"; unsigned n = 0;
    pthread_mutex_lock(&g_tr_mu);
    for (unsigned k = 0; k < g_trn; k++) if (g_tr[k].fn == li->function) { name = g_tr[k].name; n = ++g_tr[k].launches; break; }
    pthread_mutex_unlock(&g_tr_mu);
    if (n <= 3) {
        char words[3400]; size_t o = 0; int zero = 0;
        for (size_t p = 0; p < li->paramCount && p < 24 && o < sizeof words - 24; p++) {
            uint64_t v = 0; if (li->pParams && li->pParams[p]) memcpy(&v, li->pParams[p], 8);
            if (!v) zero++;
            o += (size_t)snprintf(words + o, sizeof words - o, " %llx", (unsigned long long)v);
        }
        const void *pbuf = NULL; size_t psize = 0;
        for (size_t x = 0; li->pExtras && x + 1 < li->extraCount * 2 + 2 && x < 16; x += 2) {
            const uintptr_t key = (uintptr_t)li->pExtras[x];
            if (key == 0) break;
            if (key == 1) pbuf = li->pExtras[x + 1];
            else if (key == 2 && li->pExtras[x + 1]) psize = *(const size_t *)li->pExtras[x + 1];
        }
        if (pbuf && psize) {
            o = 0; zero = 0;
            for (size_t q = 0; q + 8 <= psize && q < 8 * 160 && o < sizeof words - 24; q += 8) {
                uint64_t v; memcpy(&v, (const char *)pbuf + q, 8); if (!v) zero++;
                o += (size_t)snprintf(words + o, sizeof words - o, " %llx", (unsigned long long)v);
            }
        }
        tr("launch %s #%u grid %u,%u,%u block %u,%u,%u smem %u params %zu extras %zu zero-words %d:%s", name, n,
              li->gridDimX, li->gridDimY, li->gridDimZ, li->blockDimX, li->blockDimY, li->blockDimZ, li->sharedMemBytes,
              li->paramCount, li->extraCount, zero, words);
    }
    if (skip_match(name)) { tr("SKIPPED launch %s", name); return; }
    r_launch(cb, li);
}

/* A name the Vulkan driver does not provide comes back NULL and the core calls through it; log each one. */
static PFN_vkVoidFunction VKAPI_CALL log_gipa(VkInstance i, const char *n)
{ PFN_vkVoidFunction f = g_gipa(i, n); if (!f) nvlog("dlss: vulkan instance-level %s is missing", n); return f; }
static PFN_vkVoidFunction trace_wrap(const char *n, PFN_vkVoidFunction f);
static PFN_vkVoidFunction VKAPI_CALL log_gdpa(VkDevice d, const char *n)
{
    PFN_vkVoidFunction f = g_gdpa(d, n);
    if (!f) { nvlog("dlss: vulkan device-level %s is missing", n); return f; }
    return trace_wrap(n, f);
}
/* the core can also take these names straight from the loader (hk_dlsym): the same wrappers apply there */
static PFN_vkVoidFunction trace_wrap(const char *n, PFN_vkVoidFunction f)
{
    if (!f || !trace_on()) return f;
    if (!strcmp(n, "vkGetBufferDeviceAddress") || !strcmp(n, "vkGetBufferDeviceAddressKHR") || !strcmp(n, "vkGetBufferDeviceAddressEXT"))
        { r_bda = (PFN_vkGetBufferDeviceAddress)f; return (PFN_vkVoidFunction)w_bda; }
    if (!strcmp(n, "vkGetImageViewHandleNVX")) { r_ivh = (PFN_vkGetImageViewHandleNVX)f; return (PFN_vkVoidFunction)w_ivh; }
    if (!strcmp(n, "vkGetImageViewHandle64NVX")) { r_ivh64 = (PFN_vkGetImageViewHandle64NVX)f; return (PFN_vkVoidFunction)w_ivh64; }
    if (!strcmp(n, "vkGetImageViewAddressNVX")) { r_iva = (PFN_vkGetImageViewAddressNVX)f; return (PFN_vkVoidFunction)w_iva; }
    if (!strcmp(n, "vkCreateCuFunctionNVX")) { r_cfn = (PFN_vkCreateCuFunctionNVX)f; return (PFN_vkVoidFunction)w_cfn; }
    if (!strcmp(n, "vkCmdDispatch")) { r_disp = (PFN_vkCmdDispatch)f; return (PFN_vkVoidFunction)w_disp; }
    if (!strcmp(n, "vkCmdPushDescriptorSetKHR")) { r_pds = (PFN_vkCmdPushDescriptorSetKHR)f; return (PFN_vkVoidFunction)w_pds; }
    if (!strcmp(n, "vkCmdBindPipeline")) { r_bp = (PFN_vkCmdBindPipeline)f; return (PFN_vkVoidFunction)w_bp; }
    if (!strcmp(n, "vkCmdCuLaunchKernelNVX")) { r_launch = (PFN_vkCmdCuLaunchKernelNVX)f; return (PFN_vkVoidFunction)w_launch; }
    return f;
}

static void *hk_dlopen(void *c, const char *f, int fl)
{
    if (strstr(f, "libnvidia-ml.so")) return NVML_H;
    if (strstr(f, "libvulkan.so")) return VK_H;
    return NULL;
}
static void *hk_dlsym(void *c, void *h, const char *n)
{
    if (h == VK_H) {
        if (!strcmp(n, "vkGetInstanceProcAddr")) return (void *)log_gipa;
        if (!strcmp(n, "vkGetDeviceProcAddr")) return (void *)log_gdpa;
        void *p = (void *)g_gipa(g_inst, n);
        if (!p) p = (void *)g_gipa(NULL, n);
        return (void *)trace_wrap(n, (PFN_vkVoidFunction)p);
    }
    if (h != NVML_H) return NULL;
    for (size_t i = 0; i < sizeof k_nvml / sizeof k_nvml[0]; i++) if (!strcmp(k_nvml[i].n, n)) return k_nvml[i].f;
    nvlog("dlss: management-library call %s answered NOT_SUPPORTED", n);
    return (void *)nv_unsupported;
}
static const elfhost_resolver k_hooks = { NULL, hk_dlopen, hk_dlsym };
static void ngx_log(const char *m, int l, int f) { size_t n = strlen(m); nvlog("dlss: ngx: %.*s", (int)(n && m[n - 1] == '\n' ? n - 1 : n), m); }

/* ---- init ---------------------------------------------------------------------------------------------- */
static int fail(const char *why) { nvlog("dlss: unavailable - %s", why); g_state = -1; return -1; }
static int init_locked(VkInstance inst, VkPhysicalDevice pd, VkDevice dev, PFN_vkGetInstanceProcAddr gipa, PFN_vkGetDeviceProcAddr gdpa,
                       uint32_t pci_vendor, uint32_t pci_device)
{
    if (!nvmtl_dlss_wanted()) { g_state = -1; return -1; }
    g_inst = inst; g_dev = dev; g_gipa = gipa; g_gdpa = gdpa; g_vendor = pci_vendor; g_device = pci_device;
    if (pci_vendor != 0x10de) return fail("not an NVIDIA device");
    g_chip = chip_for(pci_device);
    if (!g_chip) return fail("device id has no row in the chip table");
    /* the device must have been created with both extensions; asking is cheaper than a crash inside the core */
    if (!gdpa(dev, "vkCreateCuModuleNVX") || !gdpa(dev, "vkGetImageViewHandleNVX")) return fail("device lacks VK_NVX_binary_import / VK_NVX_image_view_handle");
    char core[512], err[256] = "";
    if (find_core(core, sizeof core)) return fail("NGX core not found");
    g_core = elfhost_load(core, &k_hooks, NULL, err, sizeof err);
    if (!g_core) { char m[400]; snprintf(m, sizeof m, "NGX core did not load: %s", err); return fail(m); }
    typedef unsigned (*init2_t)(unsigned long long, const wchar_t *, void *, void *, void *, void *, void *, int, const void *);
    init2_t init2 = elfhost_sym(g_core, "NVSDK_NGX_VULKAN_Init_Ext2");
    unsigned (*caps_fn)(ngxp **) = elfhost_sym(g_core, "NVSDK_NGX_VULKAN_GetCapabilityParameters");
    g_alloc = elfhost_sym(g_core, "NVSDK_NGX_VULKAN_AllocateParameters");
    g_create = elfhost_sym(g_core, "NVSDK_NGX_VULKAN_CreateFeature1");
    g_eval = elfhost_sym(g_core, "NVSDK_NGX_VULKAN_EvaluateFeature");
    if (!init2 || !caps_fn || !g_alloc || !g_create || !g_eval) return fail("NGX core lacks an expected entry point");
    static wchar_t wpath[1024]; mbstowcs(wpath, NVMTL_DLSS_SNIPPET_DIR, 1023); static const wchar_t *paths[1] = { wpath };
    struct { const wchar_t *const *p; unsigned n; void *in; void (*cb)(const char *, int, int); int lvl; bool off; } info = { paths, 1, NULL, ngx_log, 1, 0 };
    /* application id: arbitrary non-zero; data path: scratch; 0x15 is the SDK version word accepted on the proving run. */
    /* NGX writes logs and model data under this folder: a per-user cache folder that only this user owns, never a
     * shared, world-writable /tmp where another account could plant links. */
    wchar_t wdata[600]; char data[512] = "";
    size_t dn = confstr(_CS_DARWIN_USER_CACHE_DIR, data, sizeof data);
    if (!dn || dn >= sizeof data - 16) return fail("no per-user cache folder");
    strlcat(data, "nullmoth-dlss", sizeof data);
    struct stat dst;
    if (mkdir(data, 0700) && errno != EEXIST) return fail("cannot create the per-user DLSS folder");
    if (lstat(data, &dst) || !S_ISDIR(dst.st_mode) || dst.st_uid != getuid() || (dst.st_mode & 077))
        return fail("the per-user DLSS folder is not a private folder owned by this user");
    if (mbstowcs(wdata, data, sizeof wdata / sizeof wdata[0]) == (size_t)-1) return fail("DLSS folder path is not valid text");
    unsigned rc = init2(0x4e756c6cULL, wdata, inst, pd, dev, (void *)log_gipa, (void *)log_gdpa, 0x15, &info);
    if (rc != 1) { char m[64]; snprintf(m, sizeof m, "NGX Init_Ext2 returned 0x%08x", rc); return fail(m); }
    ngxp *caps = NULL; int avail = -1; caps_fn(&caps);
    if (caps) P_GETI(caps, "SuperSampling.Available", &avail);
    if (avail != 1) return fail("NGX reports Super Resolution not available");
    /* The core names Ray Reconstruction "SuperSamplingDenoising". Frame Generation is only reported: it is not driven. */
    if (caps) { P_GETI(caps, "SuperSamplingDenoising.Available", &g_rr_avail); P_GETI(caps, "FrameGeneration.Available", &g_fg_avail); }
    nvlog("dlss: core reports Ray Reconstruction %s, Frame Generation %s", g_rr_avail == 1 ? "available" : "not available",
          g_fg_avail == 1 ? "available" : "not available");
    g_state = 1;
    nvlog("dlss: ready on %s (device %04x, chip code 0x%x)", g_chip->name, pci_device, g_chip->chip);
    return 0;
}
static int init_impl(VkInstance inst, VkPhysicalDevice pd, VkDevice dev, PFN_vkGetInstanceProcAddr gipa, PFN_vkGetDeviceProcAddr gdpa,
                     uint32_t pci_vendor, uint32_t pci_device)
{
    pthread_mutex_lock(&g_lock);
    int r = g_state ? (g_state == 1 ? 0 : -1) : init_locked(inst, pd, dev, gipa, gdpa, pci_vendor, pci_device);
    pthread_mutex_unlock(&g_lock);
    return r;
}
int nvmtl_dlss_ready(void) { return g_state == 1; }

/* ---- features ---------------------------------------------------------------------------------------------- */
int nvmtl_dlss_quality_for_ratio(float r)
{
    if (r >= 2.5f) return 3;    /* about 1/3 per axis */
    if (r >= 1.8f) return 0;    /* 1/2 */
    if (r >= 1.6f) return 1;    /* 0.58 */
    if (r >= 1.35f) return 2;   /* 2/3 */
    if (r > 1.05f) return 4;    /* 0.77 */
    return 5;                   /* equal sizes: anti-aliasing only */
}
/* DLSS.Feature.Create.Flags bits */
enum { F_HDR = 1 << 0, F_MV_LOWRES = 1 << 1, F_MV_JITTERED = 1 << 2, F_DEPTH_INVERTED = 1 << 3, F_AUTO_EXPOSURE = 1 << 6 };

static nvmtl_dlss_feature *create_impl(VkCommandBuffer cmd, const nvmtl_dlss_cfg *cfg)
{
    if (g_state != 1 || !cmd || !cfg || !cfg->in_w || !cfg->in_h || !cfg->out_w || !cfg->out_h) return NULL;
    pthread_mutex_lock(&g_lock);
    nvmtl_dlss_feature *f = calloc(1, sizeof *f); ngxp *prm = NULL, *ev = NULL; void *handle = NULL; unsigned rc = 0;
    if (!f) goto bad;
    g_alloc(&prm);
    if (!prm) { nvlog("dlss: AllocateParameters gave nothing"); goto bad; }
    const int quality = nvmtl_dlss_quality_for_ratio((float)cfg->out_w / (float)cfg->in_w);
    P_SETUI(prm, "CreationNodeMask", 1); P_SETUI(prm, "VisibilityNodeMask", 1);
    P_SETUI(prm, "Width", cfg->in_w); P_SETUI(prm, "Height", cfg->in_h);
    P_SETUI(prm, "OutWidth", cfg->out_w); P_SETUI(prm, "OutHeight", cfg->out_h);
    P_SETI(prm, "PerfQualityValue", quality);
    P_SETI(prm, "DLSS.Feature.Create.Flags", (cfg->hdr ? F_HDR : 0) | (cfg->mv_lowres ? F_MV_LOWRES : 0) | (cfg->mv_jittered ? F_MV_JITTERED : 0)
                                           | (cfg->depth_inverted ? F_DEPTH_INVERTED : 0) | (cfg->auto_exposure ? F_AUTO_EXPOSURE : 0));
    P_SETI(prm, "DLSS.Enable.Output.Subrects", 0);
    /* Render preset: DLSS's own choice (the DLSS 4 transformer at 1.5x and above: 56.25 dB against the reference at a
     * game's 1280x720 -> 1920x1080 HDR + auto-exposure settings; the CNN preset E gives 52.40 dB there).
     * NVMTL_DLSS_PRESET=<n> pins one for every quality mode (5 = E, 6 = F, 10 = J, 11 = K). */
    if (getenv("NVMTL_DLSS_PRESET")) {
        const unsigned preset = (unsigned)strtoul(getenv("NVMTL_DLSS_PRESET"), NULL, 0);
        static const char *const pn[] = { "DLSS.Hint.Render.Preset.DLAA", "DLSS.Hint.Render.Preset.Quality",
                                          "DLSS.Hint.Render.Preset.Balanced", "DLSS.Hint.Render.Preset.Performance",
                                          "DLSS.Hint.Render.Preset.UltraPerformance", "DLSS.Hint.Render.Preset.UltraQuality" };
        for (unsigned i = 0; i < sizeof pn / sizeof pn[0]; i++) P_SETUI(prm, pn[i], preset);
        nvlog("dlss: render preset %u", preset);
    }
    rc = g_create(g_dev, cmd, 1, prm, &handle);
    if (rc != 1 || !handle) { nvlog("dlss: CreateFeature1 returned 0x%08x", rc); goto bad; }
    g_alloc(&ev);
    if (!ev) { nvlog("dlss: AllocateParameters (evaluate) gave nothing"); goto bad; }
    f->handle = handle; f->eval = ev; f->cfg = *cfg;
    pthread_mutex_unlock(&g_lock);
    nvlog("dlss: feature %ux%u -> %ux%u, quality %d, hdr %d, mv %s%s, depth %s, exposure %s", cfg->in_w, cfg->in_h, cfg->out_w, cfg->out_h,
          quality, cfg->hdr, cfg->mv_lowres ? "input-res" : "output-res", cfg->mv_jittered ? "+jitter" : "",
          cfg->depth_inverted ? "inverted" : "normal", cfg->auto_exposure ? "auto" : "texture");
    return f;
bad:
    pthread_mutex_unlock(&g_lock);
    free(f);   /* the parameter objects and any half-made feature belong to the core and are not released here */
    return NULL;
}
const nvmtl_dlss_cfg *nvmtl_dlss_feature_cfg(const nvmtl_dlss_feature *f) { return &f->cfg; }

static void fill(ngx_res_vk *r, const nvmtl_dlss_image *i, bool rw)
{
    r->type = 0; r->rw = rw;
    r->image = (ngx_image_vk){ i->view, i->image, { i->aspect, 0, 1, 0, 1 }, i->format, i->w, i->h };
}
static int eval_impl(VkCommandBuffer cmd, nvmtl_dlss_feature *f, const nvmtl_dlss_eval *e)
{
    if (g_state != 1 || !f || !cmd || !e) return -1;
    ngx_res_vk color, depth, motion, out, expo, react;
    fill(&color, &e->color, false); fill(&depth, &e->depth, false); fill(&motion, &e->motion, false); fill(&out, &e->output, true);
    ngxp *p = f->eval;
    pthread_mutex_lock(&g_lock);
    P_SETP(p, "Color", &color); P_SETP(p, "Output", &out); P_SETP(p, "Depth", &depth); P_SETP(p, "MotionVectors", &motion);
    if (e->exposure) { fill(&expo, e->exposure, false); P_SETP(p, "ExposureTexture", &expo); } else P_SETP(p, "ExposureTexture", NULL);
    if (e->reactive) { fill(&react, e->reactive, false); P_SETP(p, "DLSS.Input.Bias.Current.Color.Mask", &react); } else P_SETP(p, "DLSS.Input.Bias.Current.Color.Mask", NULL);
    P_SETF(p, "Jitter.Offset.X", e->jitter_x); P_SETF(p, "Jitter.Offset.Y", e->jitter_y); P_SETF(p, "Sharpness", 0);
    P_SETF(p, "MV.Scale.X", e->mv_scale_x); P_SETF(p, "MV.Scale.Y", e->mv_scale_y);
    P_SETF(p, "DLSS.Pre.Exposure", e->pre_exposure > 0 ? e->pre_exposure : 1.0f); P_SETF(p, "DLSS.Exposure.Scale", 1.0f);
    P_SETI(p, "Reset", e->reset ? 1 : 0);
    P_SETUI(p, "DLSS.Render.Subrect.Dimensions.Width", e->render_w); P_SETUI(p, "DLSS.Render.Subrect.Dimensions.Height", e->render_h);
    P_SETUI(p, "DLSS.Input.Color.Subrect.Base.X", e->color_x); P_SETUI(p, "DLSS.Input.Color.Subrect.Base.Y", e->color_y);
    P_SETUI(p, "DLSS.Input.Depth.Subrect.Base.X", e->depth_x); P_SETUI(p, "DLSS.Input.Depth.Subrect.Base.Y", e->depth_y);
    P_SETUI(p, "DLSS.Input.MV.Subrect.Base.X", e->motion_x); P_SETUI(p, "DLSS.Input.MV.Subrect.Base.Y", e->motion_y);
    unsigned rc = g_eval(cmd, f->handle, p, NULL);
    pthread_mutex_unlock(&g_lock);
    if (rc != 1) { nvlog("dlss: EvaluateFeature returned 0x%08x", rc); return -1; }
    return 0;
}

/* ---- Ray Reconstruction (the core's "SuperSamplingDenoising") ------------------------------------------------- */
/* NVSDK_NGX_Feature_RayReconstruction. Believed to be 13 (Super Resolution is 1, Frame Generation 11); the value is
 * overridable with NVMTL_DLSS_RR_FEATURE until a run on the device confirms it. */
static int rr_feature_id(void)
{
    const char *e = getenv("NVMTL_DLSS_RR_FEATURE");
    return e && e[0] ? (int)strtol(e, NULL, 0) : 13;
}
int nvmtl_dlss_rr_ready(void)
{
    const char *off = getenv("NVMTL_DLSS_RR");
    return g_state == 1 && g_rr_avail == 1 && !(off && off[0] == '0');
}
static nvmtl_dlss_rr_feature *rr_create_impl(VkCommandBuffer cmd, const nvmtl_dlss_rr_cfg *cfg)
{
    if (!nvmtl_dlss_rr_ready() || !cmd || !cfg || !cfg->in_w || !cfg->in_h || !cfg->out_w || !cfg->out_h) return NULL;
    pthread_mutex_lock(&g_lock);
    nvmtl_dlss_rr_feature *f = calloc(1, sizeof *f); ngxp *prm = NULL, *ev = NULL; void *handle = NULL;
    if (!f) goto bad;
    g_alloc(&prm);
    if (!prm) { nvlog("dlss: RR AllocateParameters gave nothing"); goto bad; }
    const int quality = nvmtl_dlss_quality_for_ratio((float)cfg->out_w / (float)cfg->in_w);
    P_SETUI(prm, "CreationNodeMask", 1); P_SETUI(prm, "VisibilityNodeMask", 1);
    P_SETUI(prm, "Width", cfg->in_w); P_SETUI(prm, "Height", cfg->in_h);
    P_SETUI(prm, "OutWidth", cfg->out_w); P_SETUI(prm, "OutHeight", cfg->out_h);
    P_SETI(prm, "PerfQualityValue", quality);
    /* The runtime states "HDR Color required" and "Low resolution Motion Vectors required": both flags are always set. */
    P_SETI(prm, "DLSS.Feature.Create.Flags", F_HDR | F_MV_LOWRES | (cfg->mv_jittered ? F_MV_JITTERED : 0)
                                           | (cfg->depth_inverted ? F_DEPTH_INVERTED : 0) | (cfg->auto_exposure ? F_AUTO_EXPOSURE : 0));
    
    P_SETI(prm, "DLSS.Enable.Output.Subrects", 0);
    /* The core refuses a Ray Reconstruction feature whose denoise mode is Off ("DenoiseModeOff is incompatible with
     * SuperSamplingDenoising features"); 1 = NVSDK_NGX_DLSS_Denoise_Mode_DLUnified. */
    P_SETI(prm, "DLSS.Denoise.Mode", 1);
    /* 0 = roughness in its own texture (MetalFX hands it over that way); 1 = packed in the normals' alpha. */
    P_SETI(prm, "DLSS.Roughness.Mode", 0);
    if (getenv("NVMTL_DLSS_RR_PRESET")) {
        const unsigned preset = (unsigned)strtoul(getenv("NVMTL_DLSS_RR_PRESET"), NULL, 0);
        static const char *const pn[] = { "RayReconstruction.Hint.Render.Preset.DLAA", "RayReconstruction.Hint.Render.Preset.Quality",
                                          "RayReconstruction.Hint.Render.Preset.Balanced", "RayReconstruction.Hint.Render.Preset.Performance",
                                          "RayReconstruction.Hint.Render.Preset.UltraPerformance", "RayReconstruction.Hint.Render.Preset.UltraQuality" };
        for (unsigned i = 0; i < sizeof pn / sizeof pn[0]; i++) P_SETUI(prm, pn[i], preset);
        nvlog("dlss: RR render preset %u", preset);
    }
    const unsigned rc = g_create(g_dev, cmd, rr_feature_id(), prm, &handle);
    if (rc != 1 || !handle) { nvlog("dlss: RR CreateFeature1 returned 0x%08x (feature id %d)", rc, rr_feature_id()); goto bad; }
    g_alloc(&ev);
    if (!ev) { nvlog("dlss: RR AllocateParameters (evaluate) gave nothing"); goto bad; }
    f->handle = handle; f->eval = ev; f->cfg = *cfg;
    pthread_mutex_unlock(&g_lock);
    nvlog("dlss: RR feature %ux%u -> %ux%u, quality %d, hdr %d, depth %s, exposure %s", cfg->in_w, cfg->in_h, cfg->out_w, cfg->out_h,
          quality, cfg->hdr, cfg->depth_inverted ? "inverted" : "normal", cfg->auto_exposure ? "auto" : "texture");
    return f;
bad:
    pthread_mutex_unlock(&g_lock);
    free(f);
    return NULL;
}
const nvmtl_dlss_rr_cfg *nvmtl_dlss_rr_feature_cfg(const nvmtl_dlss_rr_feature *f) { return &f->cfg; }
static int rr_eval_impl(VkCommandBuffer cmd, nvmtl_dlss_rr_feature *f, const nvmtl_dlss_rr_eval *e)
{
    if (!nvmtl_dlss_rr_ready() || !f || !cmd || !e) return -1;
    ngx_res_vk color, depth, motion, out, diff, spec, nrm, rgh, hit, expo, react;
    fill(&color, &e->color, false); fill(&depth, &e->depth, false); fill(&motion, &e->motion, false); fill(&out, &e->output, true);
    fill(&diff, &e->diffuse_albedo, false); fill(&spec, &e->specular_albedo, false); fill(&nrm, &e->normals, false);
    fill(&rgh, &e->roughness, false);
    ngxp *p = f->eval;
    pthread_mutex_lock(&g_lock);
    P_SETP(p, "Color", &color); P_SETP(p, "Output", &out); P_SETP(p, "Depth", &depth); P_SETP(p, "MotionVectors", &motion);
    P_SETP(p, "DLSS.Input.DiffuseAlbedo", &diff); P_SETP(p, "DLSS.Input.SpecularAlbedo", &spec);
    P_SETP(p, "GBuffer.Normals", &nrm); P_SETP(p, "GBuffer.Roughness", &rgh);
    if (e->hit_distance) { fill(&hit, e->hit_distance, false); P_SETP(p, "DLSSD.SpecularHitDistance", &hit); } else P_SETP(p, "DLSSD.SpecularHitDistance", NULL);
    if (e->exposure) { fill(&expo, e->exposure, false); P_SETP(p, "ExposureTexture", &expo); } else P_SETP(p, "ExposureTexture", NULL);
    if (e->reactive) { fill(&react, e->reactive, false); P_SETP(p, "DLSS.Input.Bias.Current.Color.Mask", &react); } else P_SETP(p, "DLSS.Input.Bias.Current.Color.Mask", NULL);
    /* The camera matrices are read through pointers, 16 floats each, so they are copied into the feature (the core may
     * read them after this call returns, when the recorded work runs). */
    memcpy(f->world_to_view, e->world_to_view, sizeof f->world_to_view); memcpy(f->view_to_clip, e->view_to_clip, sizeof f->view_to_clip);
    P_SETP(p, "WorldToViewMatrix", f->world_to_view); P_SETP(p, "ViewToClipMatrix", f->view_to_clip);
    P_SETF(p, "Jitter.Offset.X", e->jitter_x); P_SETF(p, "Jitter.Offset.Y", e->jitter_y);
    P_SETF(p, "MV.Scale.X", e->mv_scale_x); P_SETF(p, "MV.Scale.Y", e->mv_scale_y);
    P_SETF(p, "DLSS.Pre.Exposure", e->pre_exposure > 0 ? e->pre_exposure : 1.0f); P_SETF(p, "DLSS.Exposure.Scale", 1.0f);
    P_SETI(p, "Reset", e->reset ? 1 : 0);
    P_SETUI(p, "DLSS.Render.Subrect.Dimensions.Width", e->render_w); P_SETUI(p, "DLSS.Render.Subrect.Dimensions.Height", e->render_h);
    const unsigned rc = g_eval(cmd, f->handle, p, NULL);
    pthread_mutex_unlock(&g_lock);
    if (rc != 1) { nvlog("dlss: RR EvaluateFeature returned 0x%08x", rc); return -1; }
    return 0;
}

/* ---- Frame Generation (the core's "FrameGeneration", the dlssg snippet) ----------------------------------------- */
/* NVSDK_NGX_Feature_FrameGeneration. Believed to be 11; overridable with NVMTL_DLSS_FG_FEATURE until a run confirms it.
 * The snippet launches compute kernels through VK_NVX_binary_import like the other two (no optical-flow hardware is
 * named anywhere in it); the core's own error text says Width and Height are the required creation parameters. */
static int fg_feature_id(void)
{
    const char *e = getenv("NVMTL_DLSS_FG_FEATURE");
    return e && e[0] ? (int)strtol(e, NULL, 0) : 11;
}
int nvmtl_dlss_fg_ready(void)
{
    const char *off = getenv("NVMTL_DLSS_FG");
    return g_state == 1 && g_fg_avail == 1 && !(off && off[0] == '0');
}
static nvmtl_dlss_fg_feature *fg_create_impl(VkCommandBuffer cmd, const nvmtl_dlss_fg_cfg *cfg)
{
    if (!nvmtl_dlss_fg_ready() || !cmd || !cfg || !cfg->out_w || !cfg->out_h || !cfg->in_w || !cfg->in_h) return NULL;
    pthread_mutex_lock(&g_lock);
    nvmtl_dlss_fg_feature *f = calloc(1, sizeof *f); ngxp *prm = NULL, *ev = NULL; void *handle = NULL;
    if (!f) goto bad;
    g_alloc(&prm);
    if (!prm) { nvlog("dlss: FG AllocateParameters gave nothing"); goto bad; }
    P_SETUI(prm, "CreationNodeMask", 1); P_SETUI(prm, "VisibilityNodeMask", 1);
    P_SETUI(prm, "DLSSG.Width", cfg->out_w); P_SETUI(prm, "DLSSG.Height", cfg->out_h);
    P_SETUI(prm, "DLSSG.InternalWidth", cfg->in_w); P_SETUI(prm, "DLSSG.InternalHeight", cfg->in_h);
    P_SETUI(prm, "DLSSG.BackbufferFormat", cfg->color_format);
    P_SETI(prm, "DLSSG.ColorBuffersHDR", cfg->hdr); P_SETI(prm, "DLSSG.DepthInverted", cfg->depth_inverted);
    P_SETI(prm, "DLSSG.DynamicResolution", 0);
    const unsigned rc = g_create(g_dev, cmd, fg_feature_id(), prm, &handle);
    if (rc != 1 || !handle) {
        nvlog("dlss: FG CreateFeature1 returned 0x%08x (feature id %d)", rc, fg_feature_id());
        // Out of device memory (0xBAD0000D): list every app heap once - size, used, free ranges - so the run shows
        // whether the heaps the game asked for are full or mostly empty when the card ran out.
        static int heaps_listed;
        if (rc == 0xBAD0000Du && !heaps_listed++) { nvmtl_heaps_dump(NULL, dlss_heap_line); nvmtl_heap_lazy_census("at FG out-of-memory"); }
        goto bad;
    }
    g_alloc(&ev);
    if (!ev) { nvlog("dlss: FG AllocateParameters (evaluate) gave nothing"); goto bad; }
    f->handle = handle; f->eval = ev; f->cfg = *cfg;
    pthread_mutex_unlock(&g_lock);
    nvlog("dlss: FG feature %ux%u (depth/motion %ux%u), format %u, hdr %d, depth %s", cfg->out_w, cfg->out_h, cfg->in_w, cfg->in_h,
          cfg->color_format, cfg->hdr, cfg->depth_inverted ? "inverted" : "normal");
    return f;
bad:
    pthread_mutex_unlock(&g_lock);
    free(f);
    return NULL;
}
const nvmtl_dlss_fg_cfg *nvmtl_dlss_fg_feature_cfg(const nvmtl_dlss_fg_feature *f) { return &f->cfg; }
static int fg_eval_impl(VkCommandBuffer cmd, nvmtl_dlss_fg_feature *f, const nvmtl_dlss_fg_eval *e)
{
    if (!nvmtl_dlss_fg_ready() || !f || !cmd || !e) return -1;
    ngx_res_vk color, depth, motion, out, ui;
    fill(&color, &e->color, false); fill(&depth, &e->depth, false); fill(&motion, &e->motion, false); fill(&out, &e->output, true);
    ngxp *p = f->eval;
    pthread_mutex_lock(&g_lock);
    P_SETP(p, "DLSSG.Backbuffer", &color); P_SETP(p, "DLSSG.Depth", &depth); P_SETP(p, "DLSSG.MVecs", &motion);
    P_SETP(p, "DLSSG.OutputInterpolated", &out);
    if (e->ui) { fill(&ui, e->ui, false); P_SETP(p, "DLSSG.UI", &ui); } else P_SETP(p, "DLSSG.UI", NULL);
    P_SETP(p, "DLSSG.HUDLess", NULL); P_SETP(p, "DLSSG.UIAlpha", NULL); P_SETP(p, "DLSSG.OutputReal", NULL);
    /* The matrices are read through pointers: keep a copy in the feature for as long as the recorded work may run. */
    memcpy(f->clip_to_prev, e->clip_to_prev, sizeof f->clip_to_prev); memcpy(f->prev_to_clip, e->prev_to_clip, sizeof f->prev_to_clip);
    memcpy(f->view_to_clip, e->view_to_clip, sizeof f->view_to_clip);
    P_SETP(p, "DLSSG.ClipToPrevClip", f->clip_to_prev); P_SETP(p, "DLSSG.PrevClipToClip", f->prev_to_clip);
    P_SETP(p, "DLSSG.CameraViewToClip", f->view_to_clip);
    P_SETF(p, "DLSSG.CameraNear", e->near_z); P_SETF(p, "DLSSG.CameraFar", e->far_z);
    P_SETF(p, "DLSSG.CameraFOV", e->fov_y); P_SETF(p, "DLSSG.CameraAspectRatio", e->aspect);
    P_SETF(p, "DLSSG.CameraPosX", e->cam_pos[0]); P_SETF(p, "DLSSG.CameraPosY", e->cam_pos[1]); P_SETF(p, "DLSSG.CameraPosZ", e->cam_pos[2]);
    P_SETF(p, "DLSSG.CameraRightX", e->cam_right[0]); P_SETF(p, "DLSSG.CameraRightY", e->cam_right[1]); P_SETF(p, "DLSSG.CameraRightZ", e->cam_right[2]);
    P_SETF(p, "DLSSG.CameraUpX", e->cam_up[0]); P_SETF(p, "DLSSG.CameraUpY", e->cam_up[1]); P_SETF(p, "DLSSG.CameraUpZ", e->cam_up[2]);
    P_SETF(p, "DLSSG.CameraFwdX", e->cam_fwd[0]); P_SETF(p, "DLSSG.CameraFwdY", e->cam_fwd[1]); P_SETF(p, "DLSSG.CameraFwdZ", e->cam_fwd[2]);
    P_SETF(p, "DLSSG.CameraPinholeOffsetX", 0); P_SETF(p, "DLSSG.CameraPinholeOffsetY", 0);
    P_SETI(p, "DLSSG.CameraMotionIncluded", e->camera_motion_included);
    P_SETF(p, "DLSSG.MvecScaleX", e->mv_scale_x); P_SETF(p, "DLSSG.MvecScaleY", e->mv_scale_y);
    P_SETF(p, "DLSSG.JitterOffsetX", e->jitter_x); P_SETF(p, "DLSSG.JitterOffsetY", e->jitter_y);
    P_SETI(p, "DLSSG.Reset", e->reset ? 1 : 0); P_SETI(p, "DLSSG.NoWarp", 0); P_SETI(p, "DLSSG.OutputDisableInterpolation", 0);
    P_SETI(p, "DLSSG.OrthoProjection", 0); P_SETI(p, "DLSSG.MvecJittered", 0); P_SETI(p, "DLSSG.MvecDilated", 0);
    P_SETUI(p, "DLSSG.BackbufferFrameID", e->frame_id);
    P_SETUI(p, "DLSSG.MultiFrameIndex", e->multi_index); P_SETUI(p, "DLSSG.MultiFrameCount", e->multi_count);
    const unsigned rc = g_eval(cmd, f->handle, p, NULL);
    pthread_mutex_unlock(&g_lock);
    if (rc != 1) { nvlog("dlss: FG EvaluateFeature returned 0x%08x", rc); return -1; }
    return 0;
}

/* ---- the worker thread -------------------------------------------------------------------------------------- */
/* Every call that enters the NGX core or the runtime it loads runs on ONE dedicated thread with a large stack. That
 * code is built for a Linux thread (8 MB default stack); an application's worker threads here have 512 KB, and the
 * core's deep frames overran them into neighbouring memory. One thread also keeps all of the ELF host's per-thread
 * state (its thread-local blocks) in one place. The caller blocks until the job is done, so command-buffer recording
 * the core does is complete when the call returns. The thread's floating-point environment, locale and signal mask are
 * restored after every job. */
#define WORKER_STACK (16u << 20)
typedef struct { void (*fn)(void *); void *arg; int done; } wjob;
static pthread_mutex_t w_call = PTHREAD_MUTEX_INITIALIZER;   /* one job at a time */
static pthread_mutex_t w_mu = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t w_go = PTHREAD_COND_INITIALIZER, w_fin = PTHREAD_COND_INITIALIZER;
static wjob *w_job; static int w_started; static pthread_t w_thr;

static void *worker_main(void *unused)
{
    for (;;) {
        pthread_mutex_lock(&w_mu);
        while (!w_job) pthread_cond_wait(&w_go, &w_mu);
        wjob *j = w_job;
        pthread_mutex_unlock(&w_mu);
        thread_env te; env_save(&te);
        j->fn(j->arg);
        env_restore(&te);
        pthread_mutex_lock(&w_mu);
        j->done = 1; w_job = NULL;
        pthread_cond_broadcast(&w_fin);
        pthread_mutex_unlock(&w_mu);
    }
    return NULL;
}
/* 0 when the job ran, -1 when the worker could not be started. */
static int run_on_worker(void (*fn)(void *), void *arg)
{
    if (w_started && pthread_equal(pthread_self(), w_thr)) { fn(arg); return 0; }   /* already on it */
    pthread_mutex_lock(&w_call);
    if (!w_started) {
        pthread_attr_t at; pthread_attr_init(&at);
        int rc = pthread_attr_setstacksize(&at, WORKER_STACK);
        if (!rc) rc = pthread_attr_setguardsize(&at, 64 * 1024);
        if (!rc) rc = pthread_create(&w_thr, &at, worker_main, NULL);
        pthread_attr_destroy(&at);
        if (rc) { nvlog("dlss: could not start the worker thread (%d)", rc); pthread_mutex_unlock(&w_call); return -1; }
        w_started = 1;
    }
    wjob j = { fn, arg, 0 };
    pthread_mutex_lock(&w_mu);
    w_job = &j; pthread_cond_signal(&w_go);
    while (!j.done) pthread_cond_wait(&w_fin, &w_mu);
    pthread_mutex_unlock(&w_mu);
    pthread_mutex_unlock(&w_call);
    return 0;
}

typedef struct { VkInstance inst; VkPhysicalDevice pd; VkDevice dev; PFN_vkGetInstanceProcAddr gipa; PFN_vkGetDeviceProcAddr gdpa;
                 uint32_t vendor, device; int r; } init_job;
static void init_run(void *a) { init_job *j = a; j->r = init_impl(j->inst, j->pd, j->dev, j->gipa, j->gdpa, j->vendor, j->device); }
int nvmtl_dlss_init(VkInstance inst, VkPhysicalDevice pd, VkDevice dev, PFN_vkGetInstanceProcAddr gipa, PFN_vkGetDeviceProcAddr gdpa,
                    uint32_t pci_vendor, uint32_t pci_device)
{
    if (g_state) return g_state == 1 ? 0 : -1;   /* settled: no need to wake the worker */
    init_job j = { inst, pd, dev, gipa, gdpa, pci_vendor, pci_device, -1 };
    if (run_on_worker(init_run, &j)) return -1;
    return j.r;
}
typedef struct { VkCommandBuffer cmd; const nvmtl_dlss_cfg *cfg; nvmtl_dlss_feature *f; } create_job;
static void create_run(void *a) { create_job *j = a; j->f = create_impl(j->cmd, j->cfg); }
nvmtl_dlss_feature *nvmtl_dlss_feature_create(VkCommandBuffer cmd, const nvmtl_dlss_cfg *cfg)
{
    create_job j = { cmd, cfg, NULL };
    return run_on_worker(create_run, &j) ? NULL : j.f;
}
typedef struct { VkCommandBuffer cmd; nvmtl_dlss_feature *f; const nvmtl_dlss_eval *e; int r; } eval_job;
static void eval_run(void *a) { eval_job *j = a; j->r = eval_impl(j->cmd, j->f, j->e); }
int nvmtl_dlss_evaluate(VkCommandBuffer cmd, nvmtl_dlss_feature *f, const nvmtl_dlss_eval *e)
{
    eval_job j = { cmd, f, e, -1 };
    return run_on_worker(eval_run, &j) ? -1 : j.r;
}

/* Local check of the worker (no NGX needed): a job runs on a different thread, which has far more than the 512 KB an
 * application thread gets, and the caller's floating-point environment is untouched. Returns 0 when all hold. */
typedef struct { pthread_t who; size_t reached; } probe_job;
typedef struct { VkCommandBuffer cmd; const nvmtl_dlss_rr_cfg *cfg; nvmtl_dlss_rr_feature *f; } rr_create_job;
static void rr_create_run(void *a) { rr_create_job *j = a; j->f = rr_create_impl(j->cmd, j->cfg); }
nvmtl_dlss_rr_feature *nvmtl_dlss_rr_feature_create(VkCommandBuffer cmd, const nvmtl_dlss_rr_cfg *cfg)
{
    rr_create_job j = { cmd, cfg, NULL };
    if (run_on_worker(rr_create_run, &j)) return NULL;
    return j.f;
}
typedef struct { VkCommandBuffer cmd; nvmtl_dlss_rr_feature *f; const nvmtl_dlss_rr_eval *e; int r; } rr_eval_job;
static void rr_eval_run(void *a) { rr_eval_job *j = a; j->r = rr_eval_impl(j->cmd, j->f, j->e); }
int nvmtl_dlss_rr_evaluate(VkCommandBuffer cmd, nvmtl_dlss_rr_feature *f, const nvmtl_dlss_rr_eval *e)
{
    rr_eval_job j = { cmd, f, e, -1 };
    if (run_on_worker(rr_eval_run, &j)) return -1;
    return j.r;
}
typedef struct { VkCommandBuffer cmd; const nvmtl_dlss_fg_cfg *cfg; nvmtl_dlss_fg_feature *f; } fg_create_job;
static void fg_create_run(void *a) { fg_create_job *j = a; j->f = fg_create_impl(j->cmd, j->cfg); }
nvmtl_dlss_fg_feature *nvmtl_dlss_fg_feature_create(VkCommandBuffer cmd, const nvmtl_dlss_fg_cfg *cfg)
{
    fg_create_job j = { cmd, cfg, NULL };
    if (run_on_worker(fg_create_run, &j)) return NULL;
    return j.f;
}
typedef struct { VkCommandBuffer cmd; nvmtl_dlss_fg_feature *f; const nvmtl_dlss_fg_eval *e; int r; } fg_eval_job;
static void fg_eval_run(void *a) { fg_eval_job *j = a; j->r = fg_eval_impl(j->cmd, j->f, j->e); }
int nvmtl_dlss_fg_evaluate(VkCommandBuffer cmd, nvmtl_dlss_fg_feature *f, const nvmtl_dlss_fg_eval *e)
{
    fg_eval_job j = { cmd, f, e, -1 };
    if (run_on_worker(fg_eval_run, &j)) return -1;
    return j.r;
}
static size_t probe_depth(size_t used, size_t limit)
{
    volatile char frame[16 * 1024];
    frame[0] = (char)used; frame[sizeof frame - 1] = (char)used;   /* touch both ends so the pages are really used */
    if (used + sizeof frame >= limit) return used + sizeof frame;
    return probe_depth(used + sizeof frame, limit);
}
static void probe_run(void *a) { probe_job *j = a; j->who = pthread_self(); j->reached = probe_depth(0, 8u << 20); fesetround(FE_UPWARD); }
int nvmtl_dlss_selftest(void)
{
    probe_job j = {0};
    const int round = fegetround();
    if (run_on_worker(probe_run, &j)) { fprintf(stderr, "selftest: worker did not start\n"); return 1; }
    int bad = 0;
    if (pthread_equal(j.who, pthread_self())) { fprintf(stderr, "selftest: job ran on the caller's thread\n"); bad++; }
    if (j.reached < (8u << 20)) { fprintf(stderr, "selftest: only %zu bytes of stack reached\n", j.reached); bad++; }
    if (fegetround() != round) { fprintf(stderr, "selftest: caller's rounding mode changed\n"); bad++; }
    probe_job k = {0};
    run_on_worker(probe_run, &k);
    if (!pthread_equal(j.who, k.who)) { fprintf(stderr, "selftest: two jobs ran on different threads\n"); bad++; }
    fprintf(stderr, "selftest: stack reached %zu bytes, %s\n", j.reached, bad ? "FAIL" : "ok");
    return bad;
}
