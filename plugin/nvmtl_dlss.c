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
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <errno.h>
#include <unistd.h>
#include <wchar.h>

void nvlog(const char *fmt, ...);

/* The core and the runtime it loads execute on the calling thread. Their C library shims may reset the floating-point
 * environment (fesetenv), change the thread's locale, or change the signal mask; a caller that tracks those (a
 * translated game keeps its rounding and flush modes in them) must see them unchanged. */
typedef struct { fenv_t env; locale_t loc; sigset_t mask; } thread_env;
static void env_save(thread_env *t) { fegetenv(&t->env); t->loc = uselocale((locale_t)0); pthread_sigmask(SIG_SETMASK, NULL, &t->mask); }
static void env_restore(const thread_env *t) { fesetenv(&t->env); uselocale(t->loc); pthread_sigmask(SIG_SETMASK, &t->mask, NULL); }

/* ---- which device is this: PCI device id -> management-library architecture and chip code ------------------ */
/* Read from the NGX core's own code (NGX 620.70.06, function nvmlArchToNGX): it walks the GPUs, takes the NVML
 * architecture (public enum) and the chip code from the management library's private export table, slot 257, and maps
 * them to its internal architecture id:
 *   Volta 5 -> 0x140, Turing 6 -> 0x160, Ampere 7 -> 0x170, Hopper 9 -> 0x180, Ada 8 -> 0x190, anything newer than
 *   Hopper uses the chip code (Blackwell, 10): chip 0x51-0x53 and 0x5e-0x5f -> 0x1a0, every other chip -> 0x1b0.
 * For Turing, Ampere and Ada the chip code is read but not used, so only a successful answer matters. For Blackwell,
 * 0x60 selects 0x1b0, the id the core logged on a GeForce RTX 5060 (device id 0x2d05). What the real management library
 * returns in slot 257 per GPU is not decoded, so the exact chip code of each Blackwell part is unknown; any code outside
 * the two ranges above gives the same result for GeForce parts. Architecture values below are the public enum. */
typedef struct { uint16_t lo, hi; unsigned arch; int chip; const char *name; } chip_row;
static const chip_row k_chips[] = {
    { 0x2900, 0x2fff, 10, 0x60, "Blackwell (GB20x)" },   /* chip 0x60 measured on 0x2d05; maps to 0x1b0 */
    { 0x2600, 0x28ff,  8, 0x60, "Ada (AD10x)" },         /* chip unused by the core for arch 8; maps to 0x190 */
    { 0x2200, 0x25ff,  7, 0x60, "Ampere (GA10x)" },      /* chip unused for arch 7; maps to 0x170 */
    { 0x2000, 0x20ff,  7, 0x60, "Ampere (GA100)" },      /* same */
    { 0x2180, 0x21ff,  6, 0x60, "Turing (TU11x)" },      /* chip unused for arch 6; maps to 0x160 */
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

/* A name the Vulkan driver does not provide comes back NULL and the core calls through it; log each one. */
static PFN_vkVoidFunction VKAPI_CALL log_gipa(VkInstance i, const char *n)
{ PFN_vkVoidFunction f = g_gipa(i, n); if (!f) nvlog("dlss: vulkan instance-level %s is missing", n); return f; }
static PFN_vkVoidFunction VKAPI_CALL log_gdpa(VkDevice d, const char *n)
{ PFN_vkVoidFunction f = g_gdpa(d, n); if (!f) nvlog("dlss: vulkan device-level %s is missing", n); return f; }

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
        return p ? p : (void *)g_gipa(NULL, n);
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
