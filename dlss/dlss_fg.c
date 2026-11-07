/* dlss_fg - DLSS Frame Generation on the real GPU through NVK, NVIDIA's NGX core and the DLSS-G runtime, run on macOS
 * (the NGX core and the runtime through elfhost). Derived from dlss_gpu.c. Two synthetic real frames (the second is the
 * first with its content moved right by FG_SHIFT pixels, with matching motion vectors), then the in-between frame the
 * core writes is read back and judged against the first frame moved by half the shift. Controls: it must be nearer the
 * midpoint than either real frame, and a blank output must not pass.
 * Usage: dlss_fg <libvulkan_nouveau.dylib> <libnvidia-ngx.so> <snippet dir>
 * Environment: FG_SIZE, FG_FEATURE, FG_SHIFT, FG_MULTI, DLSS_TRACE. */
#include "elfhost.h"
#include <vulkan/vulkan.h>
#include <dlfcn.h>
#include <math.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <wchar.h>

#define CK(x) do { VkResult r_ = (x); if (r_ != VK_SUCCESS) { printf("FAIL %s = %d (line %d)\n", #x, r_, __LINE__); exit(1); } } while (0)
static PFN_vkGetInstanceProcAddr gipa; static PFN_vkGetDeviceProcAddr gdpa;
static VkInstance inst; static VkPhysicalDevice pd; static VkDevice dev; static VkQueue q;
static uint32_t g_vendor, g_device;
#define I(f) PFN_##f p##f = (PFN_##f)gipa(inst, #f)
#define D(f) PFN_##f p##f = (PFN_##f)gdpa(dev, #f)

/* ---- NVML stand-in (public NVML API): the device read from Vulkan ---------------------------------------------- */
/* What NGX resolves, logged: a name the real driver does not provide comes back NULL, and DLSS calls through it. */
static PFN_vkVoidFunction VKAPI_CALL log_gipa(VkInstance i, const char *n)
{ PFN_vkVoidFunction f = gipa(i, n); if (!f) fprintf(stderr, "[vk] MISSING instance-level %s\n", n); return f; }
/* DLSS_TRACE=1: log every NVX call NGX makes (module/function creation, launches, view handles). */
static PFN_vkCreateCuModuleNVX r_mod; static PFN_vkCreateCuFunctionNVX r_fn; static PFN_vkCmdCuLaunchKernelNVX r_launch;
static PFN_vkGetImageViewHandleNVX r_vh; static PFN_vkGetImageViewAddressNVX r_va;
static int g_nlaunch, g_nmod, g_nfn;
static struct { void *fn; size_t n; int idx; uint8_t p[1280]; } g_lp[64]; static int g_nl;
static VkResult VKAPI_CALL t_mod(VkDevice d, const VkCuModuleCreateInfoNVX *ci, const VkAllocationCallbacks *a, VkCuModuleNVX *m)
{ VkResult r = r_mod(d, ci, a, m); g_nmod++;
  if (r) { fprintf(stderr, "[nvx] CreateCuModule size %zu -> %d\n", ci->dataSize, r);
           char pth[64]; snprintf(pth, sizeof pth, "failmod-%d.bin", g_nmod); FILE *f = fopen(pth, "wb"); if (f) { fwrite(ci->pData, 1, ci->dataSize, f); fclose(f); } }
  return r; }
static VkResult VKAPI_CALL t_fn(VkDevice d, const VkCuFunctionCreateInfoNVX *ci, const VkAllocationCallbacks *a, VkCuFunctionNVX *f)
{ VkResult r = r_fn(d, ci, a, f); g_nfn++; fprintf(stderr, "[nvx] CreateCuFunction %s -> %d fn=%p\n", ci->pName, r, (void *)*f); return r; }
static void VKAPI_CALL t_launch(VkCommandBuffer c, const VkCuLaunchInfoNVX *li)
{
    g_nlaunch++;
    { const void *pb = NULL; size_t ps = 0;   /* keep every launch's parameters, launched or not (DLSS_DUMP / DLSS_FINDVA) */
      for (size_t x = 0; li->pExtras && x < 16; x += 2) { const uintptr_t k = (uintptr_t)li->pExtras[x]; if (!k) break;
          if (k == 1) pb = li->pExtras[x + 1]; else if (k == 2 && li->pExtras[x + 1]) ps = *(const size_t *)li->pExtras[x + 1]; }
      if (pb && ps && g_nl < 64) { g_lp[g_nl].fn = (void *)li->function; g_lp[g_nl].n = ps < 1280 ? ps : 1280; memcpy(g_lp[g_nl].p, pb, g_lp[g_nl].n); g_lp[g_nl].idx = g_nlaunch; g_nl++; } }
    /* DLSS_MAXL=N: record only the first N launches, so a GPU fault can be bisected to one kernel */
    { static int maxl = -2; if (maxl == -2) maxl = getenv("DLSS_MAXL") ? atoi(getenv("DLSS_MAXL")) : -1;
      if (maxl >= 0 && g_nlaunch > maxl) { fprintf(stderr, "[nvx] launch #%d SKIPPED (DLSS_MAXL=%d)\n", g_nlaunch, maxl); return; } }
    fprintf(stderr, "[nvx] launch #%d fn=%p grid %u,%u,%u block %u,%u,%u smem %u params %zu extras %zu\n", g_nlaunch,
            (void *)li->function, li->gridDimX, li->gridDimY, li->gridDimZ, li->blockDimX, li->blockDimY, li->blockDimZ,
            li->sharedMemBytes, li->paramCount, li->extraCount);
    /* the packed parameter buffer (CU_LAUNCH_PARAM_BUFFER_POINTER = 1, _SIZE = 2, _END = 0), compared word for word
     * with the plugin's DLSS trace of the same kernels inside a game */
    const void *pbuf = NULL; size_t psize = 0;
    for (size_t x = 0; li->pExtras && x < 16; x += 2) {
        const uintptr_t key = (uintptr_t)li->pExtras[x];
        if (key == 0) break;
        if (key == 1) pbuf = li->pExtras[x + 1];
        else if (key == 2 && li->pExtras[x + 1]) psize = *(const size_t *)li->pExtras[x + 1];
    }
    if (pbuf && psize) {
        int zero = 0; fprintf(stderr, "[nvx]   params %zu B:", psize);
        for (size_t q = 0; q + 8 <= psize && q < 8 * 160; q += 8) { uint64_t v; memcpy(&v, (const char *)pbuf + q, 8); if (!v) zero++; fprintf(stderr, " %llx", (unsigned long long)v); }
        fprintf(stderr, "  (zero-words %d)\n", zero);
    }
    r_launch(c, li);
}
static uint32_t VKAPI_CALL t_vh(VkDevice d, const VkImageViewHandleInfoNVX *i)
{ uint32_t h = r_vh(d, i); fprintf(stderr, "[nvx] ImageViewHandle view %p type %d -> 0x%x\n", (void *)i->imageView, i->descriptorType, h); return h; }
typedef uint64_t (VKAPI_PTR *PFN_h64)(VkDevice, const VkImageViewHandleInfoNVX *); static PFN_h64 r_vh64;
static uint64_t VKAPI_CALL t_vh64(VkDevice d, const VkImageViewHandleInfoNVX *i)
{ uint64_t h = r_vh64(d, i); fprintf(stderr, "[nvx] ImageViewHandle64 view %p type %d -> 0x%llx\n", (void *)i->imageView, i->descriptorType, (unsigned long long)h); return h; }
static VkResult VKAPI_CALL t_va(VkDevice d, VkImageView v, VkImageViewAddressPropertiesNVX *p)
{ VkResult r = r_va(d, v, p); fprintf(stderr, "[nvx] ImageViewAddress view %p -> %d 0x%llx size %llu\n", (void *)v, r, (unsigned long long)p->deviceAddress, (unsigned long long)p->size); return r; }
/* DLSS_DUMP=1: after the frame, read back every buffer a launch's parameters point into (fp16 statistics), so a
 * layer that writes zeros or NaNs is named. NGX's device-local allocations are moved to a host-visible type. */
static VkPhysicalDeviceMemoryProperties g_mp;
static struct { VkDeviceMemory m; VkDeviceSize size; } g_mem[4096]; static int g_nmem;
static struct { VkBuffer b; VkDeviceMemory m; VkDeviceSize off; uint64_t addr; } g_buf[4096]; static int g_nbuf;
static PFN_vkAllocateMemory r_alloc; static PFN_vkBindBufferMemory r_bind; static PFN_vkBindBufferMemory2 r_bind2;
static PFN_vkGetBufferDeviceAddress r_bda;
static VkResult VKAPI_CALL t_alloc(VkDevice d, const VkMemoryAllocateInfo *ai, const VkAllocationCallbacks *a, VkDeviceMemory *m)
{
    VkMemoryAllocateInfo c = *ai;
    if (g_mp.memoryTypeCount && !(g_mp.memoryTypes[c.memoryTypeIndex].propertyFlags & VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT))
        for (uint32_t i = 0; i < g_mp.memoryTypeCount; i++)
            if ((g_mp.memoryTypes[i].propertyFlags & (VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT)) ==
                (VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT)) { c.memoryTypeIndex = i; break; }
    VkResult r = r_alloc(d, &c, a, m);
    if (r == VK_SUCCESS && g_nmem < 4096) { g_mem[g_nmem].m = *m; g_mem[g_nmem].size = c.allocationSize; g_nmem++; }
    return r;
}
static void note_bind(VkBuffer b, VkDeviceMemory m, VkDeviceSize off)
{ for (int i = 0; i < g_nbuf; i++) if (g_buf[i].b == b) { g_buf[i].m = m; g_buf[i].off = off; return; }
  if (g_nbuf < 4096) { g_buf[g_nbuf].b = b; g_buf[g_nbuf].m = m; g_buf[g_nbuf].off = off; g_buf[g_nbuf].addr = 0; g_nbuf++; } }
static VkResult VKAPI_CALL t_bind(VkDevice d, VkBuffer b, VkDeviceMemory m, VkDeviceSize off)
{ VkResult r = r_bind(d, b, m, off); if (!r) note_bind(b, m, off); return r; }
static VkResult VKAPI_CALL t_bind2(VkDevice d, uint32_t n, const VkBindBufferMemoryInfo *bi)
{ VkResult r = r_bind2(d, n, bi); if (!r) for (uint32_t i = 0; i < n; i++) note_bind(bi[i].buffer, bi[i].memory, bi[i].memoryOffset); return r; }
static VkDeviceAddress VKAPI_CALL t_bda(VkDevice d, const VkBufferDeviceAddressInfo *i)
{ VkDeviceAddress a = r_bda(d, i); for (int k = 0; k < g_nbuf; k++) if (g_buf[k].b == i->buffer) g_buf[k].addr = a; return a; }
static float h2f_(uint16_t h)
{ uint32_t s = (h >> 15) & 1, e = (h >> 10) & 31, m = h & 1023; float v;
  if (e == 0) v = ldexpf((float)m, -24); else if (e == 31) v = m ? NAN : INFINITY; else v = ldexpf((float)(m | 1024), (int)e - 25);
  return s ? -v : v; }
static void find_va(uint64_t va)
{
    for (int k = 0; k < g_nbuf; k++) {
        VkDeviceSize msize = 0; for (int j = 0; j < g_nmem; j++) if (g_mem[j].m == g_buf[k].m) msize = g_mem[j].size;
        if (!g_buf[k].addr) continue;
        const uint64_t end = g_buf[k].addr + (msize - g_buf[k].off);
        if (va + (16u << 20) >= g_buf[k].addr && va <= end + (16u << 20))
            fprintf(stderr, "[findva] buffer 0x%llx..0x%llx (%llu B) %s\n", (unsigned long long)g_buf[k].addr, (unsigned long long)end,
                    (unsigned long long)(end - g_buf[k].addr), va >= g_buf[k].addr && va < end ? "CONTAINS" : "near");
    }
    for (int l = 0; l < g_nl; l++) for (size_t q = 0; q + 8 <= g_lp[l].n; q += 8) { uint64_t v; memcpy(&v, g_lp[l].p + q, 8);
        if (v + (16u << 20) >= va && v <= va + (16u << 20)) fprintf(stderr, "[findva] L%d +%zu = 0x%llx (va %+lld)\n", g_lp[l].idx, q, (unsigned long long)v, (long long)(va - v)); }
}
static void dump_launches(void)
{
    D(vkMapMemory); D(vkUnmapMemory);
    for (int l = 0; l < g_nl; l++) {
        for (size_t q = 0; q + 8 <= g_lp[l].n; q += 8) {
            uint64_t v; memcpy(&v, g_lp[l].p + q, 8);
            for (int k = 0; k < g_nbuf; k++) {
                VkDeviceSize msize = 0;
                for (int j = 0; j < g_nmem; j++) if (g_mem[j].m == g_buf[k].m) msize = g_mem[j].size;
                if (!g_buf[k].addr || v < g_buf[k].addr || v >= g_buf[k].addr + (msize - g_buf[k].off)) continue;
                const VkDeviceSize start = g_buf[k].off + (v - g_buf[k].addr), len = msize - start < (64u << 20) ? msize - start : (64u << 20);
                void *map = NULL;
                if (pvkMapMemory(dev, g_buf[k].m, start, len, 0, &map) != VK_SUCCESS || !map) { fprintf(stderr, "[dump] L%d +%zu 0x%llx: not mappable\n", g_lp[l].idx, q, (unsigned long long)v); break; }
                const uint16_t *h = map; size_t nh = len / 2, nz = 0, nan = 0; double amax = 0, asum = 0;
                for (size_t x = 0; x < nh; x++) { if (h[x]) nz++; float f = h2f_(h[x]); if (!isfinite(f)) { nan++; continue; } double af = fabs(f); asum += af; if (af > amax) amax = af; }
                fprintf(stderr, "[dump] L%d +%zu 0x%llx: %llu B, nonzero %.2f%%, nan/inf %zu, max|x| %.4g, mean|x| %.4g\n", g_lp[l].idx, q,
                        (unsigned long long)v, (unsigned long long)len, 100.0 * nz / (nh ? nh : 1), nan, amax, asum / (nh ? nh : 1));
                pvkUnmapMemory(dev, g_buf[k].m);
                break;
            }
        }
    }
}
static PFN_vkVoidFunction VKAPI_CALL log_gdpa(VkDevice d, const char *n)
{
    PFN_vkVoidFunction f = gdpa(d, n); if (!f) fprintf(stderr, "[vk] MISSING device-level %s\n", n);
    if (f && getenv("DLSS_TRACE")) {
#define W(name, real, wrap) if (!strcmp(n, #name)) { real = (void *)f; return (PFN_vkVoidFunction)wrap; }
        W(vkCreateCuModuleNVX, r_mod, t_mod) W(vkCreateCuFunctionNVX, r_fn, t_fn) W(vkCmdCuLaunchKernelNVX, r_launch, t_launch)
        W(vkGetImageViewHandleNVX, r_vh, t_vh) W(vkGetImageViewHandle64NVX, r_vh64, t_vh64) W(vkGetImageViewAddressNVX, r_va, t_va)
        if (getenv("DLSS_DUMP")) {
            W(vkAllocateMemory, r_alloc, t_alloc) W(vkBindBufferMemory, r_bind, t_bind) W(vkBindBufferMemory2, r_bind2, t_bind2)
            W(vkBindBufferMemory2KHR, r_bind2, t_bind2) W(vkGetBufferDeviceAddress, r_bda, t_bda) W(vkGetBufferDeviceAddressKHR, r_bda, t_bda)
            W(vkGetBufferDeviceAddressEXT, r_bda, t_bda)
        }
#undef W
    }
    return f;
}
static void *const NVML_H = (void *)0x4e564d4cULL, *const NVML_DEV = (void *)0x44455630ULL, *const VK_H = (void *)0x56554c4bULL;
static int nv_ok(void) { return 0; }
static int nv_count(unsigned *n) { *n = 1; return 0; }
static int nv_byidx(unsigned i, void **d) { if (i) return 2; *d = NVML_DEV; return 0; }
static int nv_arch(void *d, unsigned *a) { *a = 10; return 0; }   /* Blackwell; this harness runs on the RTX 5060 */
static int g_chip = 0x60;   /* UNVERIFIED consumer-Blackwell bucket for NVML's private table slot 257 */
static int etbl_chip(void *d, int *c) { *c = g_chip; return 0; }
static int etbl_none(void) { return 3; }
static void *g_etbl[320];
static int nv_etbl(const void **t, const void *uuid) { for (int i = 0; i < 320; i++) g_etbl[i] = (void *)etbl_none; g_etbl[257] = (void *)etbl_chip; *t = g_etbl; return 0; }
static int nv_drv(char *b, unsigned n) { snprintf(b, n, "620.70.06"); return 0; }
static int nv_name(void *d, char *b, unsigned n) { snprintf(b, n, "NVIDIA GeForce RTX 5060"); return 0; }
static int nv_pci(void *d, uint8_t *p) { memset(p, 0, 68); ((uint32_t *)(p + 16))[3] = (g_device << 16) | g_vendor; return 0; }
static int nv_unsupported(void) { return 3; }
static int nv_uuid(void *d, char *b, unsigned n) { snprintf(b, n, "GPU-%04x%04x-0000-0000-0000-000000000000", g_device, g_vendor); return 0; }
static int nv_cc(void *d, int *maj, int *min) { *maj = 12; *min = 0; return 0; }
static const char *nv_err(int e) { return "nvml stand-in"; }
static const struct { const char *n; void *f; } g_nvml[] = {
    {"nvmlInitWithFlags", nv_ok}, {"nvmlInit_v2", nv_ok}, {"nvmlShutdown", nv_ok}, {"nvmlDeviceGetCount_v2", nv_count},
    {"nvmlDeviceGetHandleByIndex_v2", nv_byidx}, {"nvmlDeviceGetArchitecture", nv_arch}, {"nvmlInternalGetExportTable", nv_etbl},
    {"nvmlSystemGetDriverVersion", nv_drv}, {"nvmlDeviceGetName", nv_name}, {"nvmlDeviceGetPciInfo_v3", nv_pci},
    {"nvmlInit", nv_ok}, {"nvmlDeviceGetCount", nv_count}, {"nvmlDeviceGetHandleByIndex", nv_byidx},
    {"nvmlDeviceGetUUID", nv_uuid}, {"nvmlDeviceGetCudaComputeCapability", nv_cc}, {"nvmlErrorString", nv_err},
    {"nvmlDeviceGetPciInfo", nv_pci}, {"nvmlDeviceGetPciInfo_v2", nv_pci},
};
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
        void *p = (void *)gipa(inst, n);
        return p ? p : (void *)gipa(NULL, n);
    }
    if (h != NVML_H) return NULL;
    for (size_t i = 0; i < sizeof g_nvml / sizeof g_nvml[0]; i++) if (!strcmp(g_nvml[i].n, n)) return g_nvml[i].f;
    fprintf(stderr, "[nvml] %s -> NOT_SUPPORTED\n", n);
    return (void *)nv_unsupported;
}
static const elfhost_resolver g_hooks = { NULL, hk_dlopen, hk_dlsym };
static void ngx_log(const char *m, int l, int f) { fprintf(stderr, "[ngx] %s%s", m, (*m && m[strlen(m) - 1] == '\n') ? "" : "\n"); }

/* ---- NGX public types, declared from the SDK documentation -------------------------------------------------- */
typedef struct { VkImageView view; VkImage image; VkImageSubresourceRange range; VkFormat format; unsigned w, h; } ngx_image_vk;
typedef struct { union { ngx_image_vk image; struct { VkBuffer b; unsigned size; } buffer; } res; int type; bool rw; } ngx_res_vk;
typedef struct { void **vt; } ngxp;   /* vtable: 0 SetULL 1 SetF 3 SetUI 4 SetI 7 SetVoidPointer 12 GetI */
#define P_SETF(p, n, v)  ((void (*)(ngxp *, const char *, float))(p)->vt[1])((p), (n), (v))
#define P_SETUI(p, n, v) ((void (*)(ngxp *, const char *, unsigned))(p)->vt[3])((p), (n), (v))
#define P_SETI(p, n, v)  ((void (*)(ngxp *, const char *, int))(p)->vt[4])((p), (n), (v))
#define P_SETP(p, n, v)  ((void (*)(ngxp *, const char *, void *))(p)->vt[7])((p), (n), (v))
#define P_GETI(p, n, o)  ((unsigned (*)(ngxp *, const char *, int *))(p)->vt[12])((p), (n), (o))

/* ---- small Vulkan helpers ----------------------------------------------------------------------------------- */
static VkPhysicalDeviceMemoryProperties g_mp;
static uint32_t mem_type(uint32_t bits, VkMemoryPropertyFlags want)
{
    for (uint32_t i = 0; i < g_mp.memoryTypeCount; i++)
        if ((bits & (1u << i)) && (g_mp.memoryTypes[i].propertyFlags & want) == want) return i;
    printf("FAIL no memory type 0x%x for bits 0x%x\n", want, bits); exit(1);
}
typedef struct { VkImage img; VkImageView view; VkDeviceMemory mem; VkFormat fmt; uint32_t w, h; ngx_res_vk res; } img_t;
static img_t make_image(uint32_t w, uint32_t h, VkFormat fmt, bool rw)
{
    I(vkGetPhysicalDeviceMemoryProperties); (void)pvkGetPhysicalDeviceMemoryProperties;
    D(vkCreateImage); D(vkGetImageMemoryRequirements); D(vkAllocateMemory); D(vkBindImageMemory); D(vkCreateImageView);
    img_t t = { .fmt = fmt, .w = w, .h = h };
    VkImageCreateInfo ci = { VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, NULL, 0, VK_IMAGE_TYPE_2D, fmt, { w, h, 1 }, 1, 1,
        VK_SAMPLE_COUNT_1_BIT, VK_IMAGE_TILING_OPTIMAL,
        VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_STORAGE_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT,
        VK_SHARING_MODE_EXCLUSIVE, 0, NULL, VK_IMAGE_LAYOUT_UNDEFINED };
    CK(pvkCreateImage(dev, &ci, NULL, &t.img));
    VkMemoryRequirements mr; pvkGetImageMemoryRequirements(dev, t.img, &mr);
    VkMemoryAllocateInfo ai = { VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, NULL, mr.size, mem_type(mr.memoryTypeBits, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT) };
    CK(pvkAllocateMemory(dev, &ai, NULL, &t.mem)); CK(pvkBindImageMemory(dev, t.img, t.mem, 0));
    VkImageViewCreateInfo vi = { VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, NULL, 0, t.img, VK_IMAGE_VIEW_TYPE_2D, fmt, {0},
        { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 } };
    CK(pvkCreateImageView(dev, &vi, NULL, &t.view));
    t.res.type = 0; t.res.rw = rw;
    fprintf(stderr, "[img] %ux%u fmt %d rw %d view %p\n", w, h, fmt, rw, (void *)t.view);
    t.res.res.image = (ngx_image_vk){ t.view, t.img, { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 }, fmt, w, h };
    return t;
}
typedef struct { VkBuffer b; VkDeviceMemory m; void *map; VkDeviceSize size; } buf_t;
static buf_t make_buffer(VkDeviceSize size)
{
    D(vkCreateBuffer); D(vkGetBufferMemoryRequirements); D(vkAllocateMemory); D(vkBindBufferMemory); D(vkMapMemory);
    buf_t b = { .size = size };
    VkBufferCreateInfo ci = { VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, NULL, 0, size,
        VK_BUFFER_USAGE_TRANSFER_SRC_BIT | VK_BUFFER_USAGE_TRANSFER_DST_BIT, VK_SHARING_MODE_EXCLUSIVE, 0, NULL };
    CK(pvkCreateBuffer(dev, &ci, NULL, &b.b));
    VkMemoryRequirements mr; pvkGetBufferMemoryRequirements(dev, b.b, &mr);
    VkMemoryAllocateInfo ai = { VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, NULL, mr.size,
        mem_type(mr.memoryTypeBits, VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT) };
    CK(pvkAllocateMemory(dev, &ai, NULL, &b.m)); CK(pvkBindBufferMemory(dev, b.b, b.m, 0)); CK(pvkMapMemory(dev, b.m, 0, size, 0, &b.map));
    return b;
}
static VkCommandPool pool;
static VkCommandBuffer begin_cmd(void)
{
    D(vkAllocateCommandBuffers); D(vkBeginCommandBuffer);
    VkCommandBufferAllocateInfo ai = { VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, NULL, pool, VK_COMMAND_BUFFER_LEVEL_PRIMARY, 1 };
    VkCommandBuffer c; CK(pvkAllocateCommandBuffers(dev, &ai, &c));
    VkCommandBufferBeginInfo bi = { VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO, NULL, VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT, NULL };
    CK(pvkBeginCommandBuffer(c, &bi)); return c;
}
static void submit_wait(VkCommandBuffer c)
{
    D(vkEndCommandBuffer); D(vkQueueSubmit); D(vkQueueWaitIdle);
    CK(pvkEndCommandBuffer(c));
    VkSubmitInfo si = { VK_STRUCTURE_TYPE_SUBMIT_INFO, NULL, 0, NULL, NULL, 1, &c, 0, NULL };
    CK(pvkQueueSubmit(q, 1, &si, VK_NULL_HANDLE)); CK(pvkQueueWaitIdle(q));
}
static void barrier_all(VkCommandBuffer c, img_t *im, VkImageLayout from, VkImageLayout to)
{
    D(vkCmdPipelineBarrier);
    VkImageMemoryBarrier b = { VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER, NULL, VK_ACCESS_MEMORY_WRITE_BIT,
        VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT, from, to, VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED,
        im->img, { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 } };
    pvkCmdPipelineBarrier(c, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, 0, 0, NULL, 0, NULL, 1, &b);
}
static uint16_t f2h(float f)   /* float -> IEEE half, round to nearest */
{
    uint32_t x; memcpy(&x, &f, 4);
    uint32_t s = (x >> 16) & 0x8000; int e = ((x >> 23) & 0xff) - 127 + 15; uint32_t m = x & 0x7fffff;
    if (e <= 0) return (uint16_t)s; if (e >= 31) return (uint16_t)(s | 0x7c00);
    return (uint16_t)(s | (e << 10) | ((m + 0x1000) >> 13));
}
static float h2f(uint16_t h)
{
    uint32_t s = (h & 0x8000) << 16, e = (h >> 10) & 0x1f, m = h & 0x3ff, x;
    if (e == 0) x = s; else if (e == 31) x = s | 0x7f800000 | (m << 13); else x = s | ((e - 15 + 127) << 23) | (m << 13);
    float f; memcpy(&f, &x, 4); return f;
}
static float pattern(float u, float v, int ch)   /* smooth bands plus a few edges, values 0.05..0.95 */
{
    float a = 0.5f + 0.4f * sinf(6.2831853f * (u * (2 + ch) + v * 1.5f));
    float edge = ((int)(u * 8) + (int)(v * 6)) % 2 ? 0.1f : 0.0f;
    float r = a * 0.9f + edge; return r < 0.05f ? 0.05f : r > 0.95f ? 0.95f : r;
}
int main(int argc, char **argv)
{
    if (argc < 4) { fprintf(stderr, "usage: dlss_fg <nvk.dylib> <libnvidia-ngx.so> <snippet dir>\n"); return 2; }
    setvbuf(stdout, NULL, _IONBF, 0);
    void *icd = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
    if (!icd) { printf("FAIL dlopen NVK: %s\n", dlerror()); return 1; }
    gipa = (PFN_vkGetInstanceProcAddr)dlsym(icd, "vk_icdGetInstanceProcAddr");
    PFN_vkCreateInstance pvkCreateInstance = (PFN_vkCreateInstance)gipa(NULL, "vkCreateInstance");
    VkApplicationInfo app = { VK_STRUCTURE_TYPE_APPLICATION_INFO, NULL, "dlss_fg", 1, "nullmoth", 1, VK_API_VERSION_1_3 };
    VkInstanceCreateInfo ici = { VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, NULL, 0, &app, 0, NULL, 0, NULL };
    CK(pvkCreateInstance(&ici, NULL, &inst));
    I(vkEnumeratePhysicalDevices); I(vkGetPhysicalDeviceProperties); I(vkGetPhysicalDeviceFeatures2); I(vkCreateDevice);
    I(vkGetPhysicalDeviceMemoryProperties); I(vkEnumerateDeviceExtensionProperties);
    gdpa = (PFN_vkGetDeviceProcAddr)gipa(inst, "vkGetDeviceProcAddr");
    uint32_t n = 1; CK(pvkEnumeratePhysicalDevices(inst, &n, &pd));
    VkPhysicalDeviceProperties pp; pvkGetPhysicalDeviceProperties(pd, &pp); pvkGetPhysicalDeviceMemoryProperties(pd, &g_mp);
    g_vendor = pp.vendorID; g_device = pp.deviceID;
    printf("device: %s (%04x:%04x)\n", pp.deviceName, pp.vendorID, pp.deviceID);
    const char *want[] = { "VK_NVX_binary_import", "VK_NVX_image_view_handle", "VK_KHR_push_descriptor" };
    uint32_t ne = 0; pvkEnumerateDeviceExtensionProperties(pd, NULL, &ne, NULL);
    VkExtensionProperties *ep = calloc(ne, sizeof *ep); pvkEnumerateDeviceExtensionProperties(pd, NULL, &ne, ep);
    for (int w = 0; w < 3; w++) { int f = 0; for (uint32_t i = 0; i < ne; i++) f |= !strcmp(ep[i].extensionName, want[w]); printf("ext %s: %s\n", want[w], f ? "yes" : "NO"); if (!f) return 1; }
    VkPhysicalDeviceVulkan13Features f13 = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_3_FEATURES };
    VkPhysicalDeviceVulkan12Features f12 = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_2_FEATURES, &f13 };
    VkPhysicalDeviceVulkan11Features f11 = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_1_FEATURES, &f12 };
    VkPhysicalDeviceFeatures2 f2 = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2, &f11 };
    pvkGetPhysicalDeviceFeatures2(pd, &f2);   /* enable everything the device has: legal, and DLSS needs some of it */
    float prio = 1.0f;
    VkDeviceQueueCreateInfo qi = { VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO, NULL, 0, 0, 1, &prio };
    VkDeviceCreateInfo dci = { VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO, &f2, 0, 1, &qi, 0, NULL, 3, want, NULL };
    CK(pvkCreateDevice(pd, &dci, NULL, &dev));
    D(vkGetDeviceQueue); D(vkCreateCommandPool); pvkGetDeviceQueue(dev, 0, 0, &q);
    VkCommandPoolCreateInfo cpi = { VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, NULL, VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT, 0 };
    CK(pvkCreateCommandPool(dev, &cpi, NULL, &pool));

    char err[256];
    elfhost_module *core = elfhost_load(argv[2], &g_hooks, NULL, err, sizeof err);
    if (!core) { printf("FAIL NGX core load: %s\n", err); return 1; }
    typedef unsigned (*init2_t)(unsigned long long, const wchar_t *, void *, void *, void *, void *, void *, int, const void *);
    init2_t init2 = elfhost_sym(core, "NVSDK_NGX_VULKAN_Init_Ext2");
    static wchar_t wpath[1024]; mbstowcs(wpath, argv[3], 1023); static const wchar_t *paths[1] = { wpath };
    struct { const wchar_t *const *p; unsigned n; void *in; void (*cb)(const char *, int, int); int lvl; bool off; } info = { paths, 1, NULL, ngx_log, 1, 0 };
    unsigned r = init2(0x1234567ull, L"/tmp", inst, pd, dev, (void *)log_gipa, (void *)log_gdpa, 0x15, &info);
    printf("NGX Init_Ext2 = 0x%08x\n", r); if (r != 1) return 1;
    ngxp *caps = NULL, *prm = NULL, *ev = NULL; int avail = -1;
    unsigned (*alloc)(ngxp **) = elfhost_sym(core, "NVSDK_NGX_VULKAN_AllocateParameters");
    ((unsigned (*)(ngxp **))elfhost_sym(core, "NVSDK_NGX_VULKAN_GetCapabilityParameters"))(&caps);
    P_GETI(caps, "FrameGeneration.Available", &avail);
    printf("FrameGeneration.Available = %d\n", avail);
    if (avail != 1) { printf("FAIL: the core does not offer Frame Generation\n"); return 1; }
    alloc(&prm);
    /* FG_SIZE=WxH (colour and output), FG_FEATURE=<NGX feature id of Frame Generation, 11 unless overridden>,
     * FG_SHIFT=<pixels the content moves between the two frames, default 32>, FG_MULTI=<MultiFrameCount, default 1>. */
    uint32_t W = 1280, H = 720; int FEAT = 11, SHIFT = 32; unsigned MULTI = 1;
    if (getenv("FG_SIZE")) sscanf(getenv("FG_SIZE"), "%ux%u", &W, &H);
    if (getenv("FG_FEATURE")) FEAT = atoi(getenv("FG_FEATURE"));
    if (getenv("FG_SHIFT")) SHIFT = atoi(getenv("FG_SHIFT"));
    if (getenv("FG_MULTI")) MULTI = (unsigned)atoi(getenv("FG_MULTI"));
    const VkFormat BB = VK_FORMAT_R16G16B16A16_SFLOAT;
    printf("FG feature id %d: %ux%u, content moves %d px between the frames\n", FEAT, W, H, SHIFT);
    P_SETUI(prm, "CreationNodeMask", 1); P_SETUI(prm, "VisibilityNodeMask", 1);
    P_SETUI(prm, "DLSSG.Width", W); P_SETUI(prm, "DLSSG.Height", H); P_SETUI(prm, "DLSSG.InternalWidth", W); P_SETUI(prm, "DLSSG.InternalHeight", H);
    P_SETUI(prm, "DLSSG.BackbufferFormat", (unsigned)BB); P_SETI(prm, "DLSSG.ColorBuffersHDR", 1); P_SETI(prm, "DLSSG.DepthInverted", 0);
    P_SETI(prm, "DLSSG.DynamicResolution", 0);
    void *handle = NULL;
    VkCommandBuffer c = begin_cmd();
    unsigned r2 = ((unsigned (*)(void *, void *, int, ngxp *, void **))elfhost_sym(core, "NVSDK_NGX_VULKAN_CreateFeature1"))(dev, c, FEAT, prm, &handle);
    submit_wait(c);
    printf("CreateFeature1 = 0x%08x handle=%p\n", r2, handle); if (r2 != 1) return 1;

    /* Two real frames: B is A with its content moved right by SHIFT pixels. Motion vectors point from each pixel of the
     * newer frame to where it was in the older one, in pixels: (-SHIFT, 0). The in-between frame should be A moved by
     * SHIFT/2. */
    img_t colA = make_image(W, H, BB, false), colB = make_image(W, H, BB, false), depth = make_image(W, H, VK_FORMAT_R32_SFLOAT, false);
    img_t mvec = make_image(W, H, VK_FORMAT_R16G16_SFLOAT, false), outA = make_image(W, H, BB, true), outB = make_image(W, H, BB, true);
    buf_t upA = make_buffer((VkDeviceSize)W * H * 8), upB = make_buffer((VkDeviceSize)W * H * 8), upM = make_buffer((VkDeviceSize)W * H * 4);
    buf_t rb = make_buffer((VkDeviceSize)W * H * 8);
    uint16_t *pa = upA.map, *pb = upB.map, *pm = upM.map;
    for (uint32_t y = 0; y < H; y++) for (uint32_t x = 0; x < W; x++) {
        for (int ch = 0; ch < 4; ch++) {
            pa[((size_t)y * W + x) * 4 + ch] = f2h(ch == 3 ? 1.0f : pattern((x + 0.5f) / W, (y + 0.5f) / H, ch));
            pb[((size_t)y * W + x) * 4 + ch] = f2h(ch == 3 ? 1.0f : pattern((x + 0.5f - SHIFT) / W, (y + 0.5f) / H, ch));
        }
        pm[((size_t)y * W + x) * 2 + 0] = f2h((float)-SHIFT); pm[((size_t)y * W + x) * 2 + 1] = f2h(0.0f);
    }
    D(vkCmdCopyBufferToImage); D(vkCmdClearColorImage); D(vkCmdCopyImageToBuffer);
    c = begin_cmd();
    img_t *all[6] = { &colA, &colB, &depth, &mvec, &outA, &outB };
    for (int i = 0; i < 6; i++) barrier_all(c, all[i], VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_GENERAL);
    VkBufferImageCopy cp = { 0, 0, 0, { VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1 }, { 0, 0, 0 }, { W, H, 1 } };
    pvkCmdCopyBufferToImage(c, upA.b, colA.img, VK_IMAGE_LAYOUT_GENERAL, 1, &cp);
    pvkCmdCopyBufferToImage(c, upB.b, colB.img, VK_IMAGE_LAYOUT_GENERAL, 1, &cp);
    pvkCmdCopyBufferToImage(c, upM.b, mvec.img, VK_IMAGE_LAYOUT_GENERAL, 1, &cp);
    VkImageSubresourceRange rr = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 };
    VkClearColorValue v_depth = { .float32 = { 0.5f, 0, 0, 0 } }, v_zero = { .float32 = { 0, 0, 0, 0 } };
    pvkCmdClearColorImage(c, depth.img, VK_IMAGE_LAYOUT_GENERAL, &v_depth, 1, &rr);
    pvkCmdClearColorImage(c, outA.img, VK_IMAGE_LAYOUT_GENERAL, &v_zero, 1, &rr);
    pvkCmdClearColorImage(c, outB.img, VK_IMAGE_LAYOUT_GENERAL, &v_zero, 1, &rr);
    for (int i = 0; i < 6; i++) barrier_all(c, all[i], VK_IMAGE_LAYOUT_GENERAL, VK_IMAGE_LAYOUT_GENERAL);
    submit_wait(c);

    static float ident[16] = { 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1 };
    static float v2c[16] = { 1.7778f, 0, 0, 0, 0, 1, 0, 0, 0, 0, -1.001f, -1, 0, 0, -0.1001f, 0 };
    alloc(&ev);
    unsigned (*evalf)(VkCommandBuffer, void *, ngxp *, void *) = elfhost_sym(core, "NVSDK_NGX_VULKAN_EvaluateFeature");
    for (int pass = 0; pass < 2; pass++) {   /* pass 0 feeds frame A (history starts), pass 1 feeds frame B and yields the in-between */
        img_t *cur = pass ? &colB : &colA, *dst = pass ? &outB : &outA;
        P_SETP(ev, "DLSSG.Backbuffer", &cur->res); P_SETP(ev, "DLSSG.Depth", &depth.res); P_SETP(ev, "DLSSG.MVecs", &mvec.res);
        P_SETP(ev, "DLSSG.OutputInterpolated", &dst->res);
        P_SETP(ev, "DLSSG.HUDLess", NULL); P_SETP(ev, "DLSSG.UI", NULL); P_SETP(ev, "DLSSG.UIAlpha", NULL); P_SETP(ev, "DLSSG.OutputReal", NULL);
        P_SETP(ev, "DLSSG.ClipToPrevClip", ident); P_SETP(ev, "DLSSG.PrevClipToClip", ident); P_SETP(ev, "DLSSG.CameraViewToClip", v2c);
        P_SETF(ev, "DLSSG.CameraNear", 0.1f); P_SETF(ev, "DLSSG.CameraFar", 1000.0f); P_SETF(ev, "DLSSG.CameraFOV", 1.0472f);
        P_SETF(ev, "DLSSG.CameraAspectRatio", (float)W / H);
        P_SETF(ev, "DLSSG.CameraPosX", 0); P_SETF(ev, "DLSSG.CameraPosY", 0); P_SETF(ev, "DLSSG.CameraPosZ", 0);
        P_SETF(ev, "DLSSG.CameraRightX", 1); P_SETF(ev, "DLSSG.CameraRightY", 0); P_SETF(ev, "DLSSG.CameraRightZ", 0);
        P_SETF(ev, "DLSSG.CameraUpX", 0); P_SETF(ev, "DLSSG.CameraUpY", 1); P_SETF(ev, "DLSSG.CameraUpZ", 0);
        P_SETF(ev, "DLSSG.CameraFwdX", 0); P_SETF(ev, "DLSSG.CameraFwdY", 0); P_SETF(ev, "DLSSG.CameraFwdZ", -1);
        P_SETF(ev, "DLSSG.CameraPinholeOffsetX", 0); P_SETF(ev, "DLSSG.CameraPinholeOffsetY", 0); P_SETI(ev, "DLSSG.CameraMotionIncluded", 1);
        P_SETF(ev, "DLSSG.MvecScaleX", 1); P_SETF(ev, "DLSSG.MvecScaleY", 1); P_SETF(ev, "DLSSG.JitterOffsetX", 0); P_SETF(ev, "DLSSG.JitterOffsetY", 0);
        P_SETI(ev, "DLSSG.Reset", pass == 0); P_SETI(ev, "DLSSG.NoWarp", 0); P_SETI(ev, "DLSSG.OutputDisableInterpolation", 0);
        P_SETI(ev, "DLSSG.OrthoProjection", 0); P_SETI(ev, "DLSSG.MvecJittered", 0); P_SETI(ev, "DLSSG.MvecDilated", 0);
        P_SETUI(ev, "DLSSG.BackbufferFrameID", (unsigned)pass + 1); P_SETUI(ev, "DLSSG.MultiFrameIndex", 1); P_SETUI(ev, "DLSSG.MultiFrameCount", MULTI);
        c = begin_cmd();
        r2 = evalf(c, handle, ev, NULL);
        printf("EvaluateFeature frame %d (recorded) = 0x%08x\n", pass + 1, r2);
        if (r2 != 1) return 1;
        barrier_all(c, dst, VK_IMAGE_LAYOUT_GENERAL, VK_IMAGE_LAYOUT_GENERAL);
        if (pass) { VkBufferImageCopy cb = { 0, 0, 0, { VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1 }, { 0, 0, 0 }, { W, H, 1 } };
                    pvkCmdCopyImageToBuffer(c, dst->img, VK_IMAGE_LAYOUT_GENERAL, rb.b, 1, &cb); }
        submit_wait(c);
    }
    printf("NVX: modules %d, functions %d, launches %d\n", g_nmod, g_nfn, g_nlaunch);

    /* Judge the in-between frame (read back from the second evaluation) against A moved by SHIFT/2. Controls: it must be
     * nearer that than frame A or frame B alone (by 3 dB), and a blank output must not pass. */
    const uint16_t *o = rb.map; size_t bad = 0, nz = 0; double se_mid = 0, se_a = 0, se_b = 0, se_zero = 0, sum = 0;
    const float half = SHIFT * 0.5f;
    for (uint32_t y = 0; y < H; y++) for (uint32_t x = 0; x < W; x++) for (int ch = 0; ch < 3; ch++) {
        float v = h2f(o[((size_t)y * W + x) * 4 + ch]);
        if (!isfinite(v)) { bad++; continue; }
        if (v != 0) nz++;
        sum += v;
        float mid = pattern((x + 0.5f - half) / W, (y + 0.5f) / H, ch), ra = pattern((x + 0.5f) / W, (y + 0.5f) / H, ch),
              rb_ = pattern((x + 0.5f - SHIFT) / W, (y + 0.5f) / H, ch);
        se_mid += (double)(v - mid) * (v - mid); se_a += (double)(v - ra) * (v - ra); se_b += (double)(v - rb_) * (v - rb_);
        se_zero += (double)mid * mid;
    }
    size_t N = (size_t)W * H * 3;
    double p_mid = 10 * log10(1.0 / (se_mid / N)), p_a = 10 * log10(1.0 / (se_a / N)), p_b = 10 * log10(1.0 / (se_b / N)), p_z = 10 * log10(1.0 / (se_zero / N));
    printf("output %ux%u: non-finite %zu, non-zero %.1f%%, mean %.3f\n", W, H, bad, 100.0 * nz / N, sum / N);
    printf("PSNR vs midpoint %.2f dB; control vs frame A %.2f dB; control vs frame B %.2f dB; control (blank output) %.2f dB\n", p_mid, p_a, p_b, p_z);
    bool pass = bad == 0 && nz > N * 0.9 && p_mid > 20 && p_mid - p_a > 3 && p_mid - p_b > 3 && p_mid - p_z > 5;
    printf("%s: DLSS Frame Generation on %s\n", pass ? "PASS" : "FAIL", pp.deviceName);
    return pass ? 0 : 1;
}
