// trace-linux/nvtrace.c (M12): an LD_PRELOAD interposer that records how a process drives NVIDIA RM through NVIDIA's
// Linux driver, to see how its NVENC client sets up an AV1 encode (observation at the ioctl boundary, no
// disassembly; the user's decision 2026-10-19, docs/tasks.md M12). Port of trace/nvtrace.c (macOS):
//   - every RM escape (ioctl 'F' on /dev/nvidia*) with its parameters and the calling library;
//   - memory objects: class, attributes, size, GPU address (MAP_MEMORY_DMA), CPU mapping (MAP_MEMORY + mmap, or the
//     user pointer of an OS-descriptor allocation);
//   - for channels on an NVENC engine: a thread follows the GPFIFO, decodes each pushed segment and at every NVENC
//     EXECUTE logs the method state and the picture setup, and dumps the buffer behind each address method to
//     NVTRACE_DIR/exec<N>-<method>.bin (whole memory object from that address, at most 16 MB).
//   LD_PRELOAD=./nvtrace.so NVTRACE_OUT=/tmp/t.log NVTRACE_DIR=/tmp/t ffmpeg ... -c:v av1_nvenc ...
// Build (Debian, against open-gpu-kernel-modules' headers): see trace-linux/build.sh
#define _GNU_SOURCE
#include <dlfcn.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>
#include <fcntl.h>
#include "nvtypes.h"
#include "nvos.h"
#include "nvstatus.h"
#include "nv-ioctl.h"
#include "nv-ioctl-numbers.h"
#include "nv_escape.h"
#include "nv-unix-nvos-params-wrappers.h"
#include "alloc/alloc_channel.h"
#include "class/cl2080_notification.h"
#include "class/clc56f.h"

static FILE *gOut;
static pthread_mutex_t gLock = PTHREAD_MUTEX_INITIALIZER;
static void L(const char *fmt, ...)
{
    if (!gOut) return;
    va_list a; va_start(a, fmt); vfprintf(gOut, fmt, a); va_end(a); fflush(gOut);
}
static void hexi(const char *ind, const void *p, size_t n)   // indented, with offsets; all-zero rows skipped
{
    const NvU32 *w = p;
    for (size_t i = 0; i < n / 4; i += 8) {
        int nz = 0; for (size_t k = i; k < i + 8 && k < n / 4; k++) nz |= w[k] != 0;
        if (!nz && n > 128) continue;
        L("%s%04zx:", ind, i * 4);
        for (size_t k = i; k < i + 8 && k < n / 4; k++) L(" %08x", w[k]);
        L("\n");
    }
}
static const char *caller(void *ra)
{
    Dl_info di;
    if (!dladdr(ra, &di) || !di.dli_fname) return "?";
    const char *s = strrchr(di.dli_fname, '/');
    return s ? s + 1 : di.dli_fname;
}

// ---- memory objects ----
typedef struct { NvHandle client, h, parent; int ctlFd; NvU32 cls, type, flags, attr, attr2, mapFlags, kind; NvU64 size; void *cpu; NvU64 cpuLen; NvU64 gpu, gpuLen; void *pendCpu; } Mem;
static Mem gMem[16384]; static int gNMem;
static Mem *mem_get(NvHandle client, NvHandle h, int create)
{
    for (int i = gNMem - 1; i >= 0; i--) if (gMem[i].client == client && gMem[i].h == h) return &gMem[i];
    if (!create || gNMem == 16384) return NULL;
    memset(&gMem[gNMem], 0, sizeof gMem[0]); gMem[gNMem].client = client; gMem[gNMem].h = h; return &gMem[gNMem++];
}
static NvHandle gVaClient;
static Mem *mem_by_gpu(NvU64 va)
{
    for (int i = gNMem - 1; i >= 0; i--)
        if (gMem[i].gpuLen && va >= gMem[i].gpu && va < gMem[i].gpu + gMem[i].gpuLen && (!gVaClient || gMem[i].client == gVaClient)) return &gMem[i];
    return NULL;
}
static void *va2cpu(NvU64 va)
{
    Mem *m = mem_by_gpu(va);
    return m && m->cpu && va - m->gpu < (m->cpuLen ? m->cpuLen : m->gpuLen) ? (NvU8 *)m->cpu + (va - m->gpu) : NULL;
}
// pending CPU maps: MAP_MEMORY names the fd that the following mmap() uses
typedef struct { int fd; NvHandle client, mem; NvU64 off, len; } PendMap;
static PendMap gPend[64]; static int gNPend;

// ---- NVENC channels ----
typedef struct {
    NvHandle client, h; NvU32 engineType; NvU64 gpfifoVa; NvU32 entries, cur; NvU64 *snap;
    NvU32 subclass[8]; NvU32 state[0x1400 / 4]; NvU8 set[0x1400 / 4]; int execs; int dec;
} Chan;
static Chan gChan[16]; static int gNChan; static int gExecs;
typedef struct { NvHandle client, h; NvU64 gpfifoVa; NvU32 entries; } ChanInfo;
static ChanInfo gAll[256]; static int gNAll;

static const char *nvenc_name(NvU32 m)
{
    switch (m) {
    case 0x000: return "SET_OBJECT"; case 0x100: return "NOP"; case 0x200: return "SET_APPLICATION_ID"; case 0x204: return "SET_WATCHDOG_TIMER";
    case 0x240: return "SEMAPHORE_A"; case 0x244: return "SEMAPHORE_B"; case 0x248: return "SEMAPHORE_C"; case 0x300: return "EXECUTE";
    case 0x304: return "SEMAPHORE_D"; case 0x480: return "SET_IN_AV1_PROBABILITY_DATA"; case 0x484: return "SET_OUT_AV1_PROBABILITY_DATA";
    case 0x488: return "SET_IOENTROPY_ABOVE_CTX_DATA"; case 0x48c: return "SET_IOENTROPY_ABOVE_MV_DATA"; case 0x490: return "SET_IODEBLOCK_TOPROW";
    case 0x494: return "SET_IODEBLOCK_LEFTCOLUMN"; case 0x498: return "SET_IOCDEF_LEFTCOLUMN"; case 0x49c: return "SET_IOLR_LEFTCOLUMN";
    case 0x4a0: return "SET_IN_SSIMRDO_INPUT_DENORM"; case 0x4a4: return "SET_IN_SSIMRDO_OUTPUT_DENORM"; case 0x4c8: return "SET_TOTAL_CORE_NUM";
    case 0x700: return "SET_CONTROL_PARAMS"; case 0x704: return "SET_PICTURE_INDEX"; case 0x708: return "SET_OUT_ENCRYPT_PARAMS";
    case 0x70c: return "SET_IN_RCDATA"; case 0x710: return "SET_IN_DRV_PIC_SETUP"; case 0x714: return "SET_IN_CEAHINTS_DATA";
    case 0x718: return "SET_OUT_ENC_STATUS"; case 0x71c: return "SET_OUT_BITSTREAM"; case 0x720: return "SET_IOHISTORY";
    case 0x724: return "SET_IO_RC_PROCESS"; case 0x728: return "SET_IN_COLOC_DATA"; case 0x72c: return "SET_OUT_COLOC_DATA";
    case 0x730: return "SET_OUT_REF_PIC_LUMA"; case 0x734: return "SET_IN_CUR_PIC"; case 0x738: return "SET_IN_MEPRED_DATA";
    case 0x73c: return "SET_OUT_MEPRED_DATA"; case 0x740: return "SET_IN_CUR_PIC_CHROMA_U"; case 0x744: return "SET_IN_CUR_PIC_CHROMA_V";
    case 0x748: return "SET_IN_QP_MAP"; case 0x74c: return "SET_OUT_REF_PIC_CHROMA"; case 0x750: return "SET_IN_PARTITION_BUF";
    case 0x768: return "SET_OUT_SCALE_REF_PIC_LUMA"; case 0x76c: return "SET_OUT_SCALE_REF_PIC_CHROMA"; case 0x774: return "SET_IO_DEBUG_STATUS";
    case 0x778: return "SET_IN_LAMBDA_MAP"; case 0x77c: return "SET_OUT_HYBRID_RES";
    }
    if (m >= 0x400 && m < 0x440) return "SET_IN_REF_PICn_LUMA";
    if (m >= 0x440 && m < 0x480) return "SET_IN_REF_PICn_CHROMA";
    if (m >= 0x4a8 && m < 0x4c8) return "SET_IN_VP9_...";
    return "";
}
static int is_addr_method(NvU32 m) { return (m >= 0x400 && m < 0x4c8) || (m >= 0x708 && m < 0x780 && m != 0x704); }

// NVDEC (classes 0xXXb0; names from open-gpu-doc clc5b0.h, Turing): 0x4c8 is Ada's SET_INTRA_TOP_BUF_OFFSET (not in clc5b0)
static const char *nvdec_name(NvU32 m)
{
    switch (m) {
    case 0x000: return "SET_OBJECT";
    case 0x100: return "NOP";
    case 0x200: return "SET_APPLICATION_ID";
    case 0x204: return "SET_WATCHDOG_TIMER";
    case 0x240: return "SEMAPHORE_A";
    case 0x244: return "SEMAPHORE_B";
    case 0x248: return "SEMAPHORE_C";
    case 0x24c: return "CTX_SAVE_AREA";
    case 0x250: return "CTX_SWITCH";
    case 0x300: return "EXECUTE";
    case 0x304: return "SEMAPHORE_D";
    case 0x400: return "SET_CONTROL_PARAMS";
    case 0x404: return "SET_DRV_PIC_SETUP_OFFSET";
    case 0x408: return "SET_IN_BUF_BASE_OFFSET";
    case 0x40c: return "SET_PICTURE_INDEX";
    case 0x410: return "SET_SLICE_OFFSETS_BUF_OFFSET";
    case 0x414: return "SET_COLOC_DATA_OFFSET";
    case 0x418: return "SET_HISTORY_OFFSET";
    case 0x41c: return "SET_DISPLAY_BUF_SIZE";
    case 0x420: return "SET_HISTOGRAM_OFFSET";
    case 0x424: return "SET_NVDEC_STATUS_OFFSET";
    case 0x428: return "SET_DISPLAY_BUF_LUMA_OFFSET";
    case 0x42c: return "SET_DISPLAY_BUF_CHROMA_OFFSET";
    case 0x430: return "SET_PICTURE_LUMA_OFFSET0";
    case 0x434: return "SET_PICTURE_LUMA_OFFSET1";
    case 0x438: return "SET_PICTURE_LUMA_OFFSET2";
    case 0x43c: return "SET_PICTURE_LUMA_OFFSET3";
    case 0x440: return "SET_PICTURE_LUMA_OFFSET4";
    case 0x444: return "SET_PICTURE_LUMA_OFFSET5";
    case 0x448: return "SET_PICTURE_LUMA_OFFSET6";
    case 0x44c: return "SET_PICTURE_LUMA_OFFSET7";
    case 0x450: return "SET_PICTURE_LUMA_OFFSET8";
    case 0x454: return "SET_PICTURE_LUMA_OFFSET9";
    case 0x458: return "SET_PICTURE_LUMA_OFFSET10";
    case 0x45c: return "SET_PICTURE_LUMA_OFFSET11";
    case 0x460: return "SET_PICTURE_LUMA_OFFSET12";
    case 0x464: return "SET_PICTURE_LUMA_OFFSET13";
    case 0x468: return "SET_PICTURE_LUMA_OFFSET14";
    case 0x46c: return "SET_PICTURE_LUMA_OFFSET15";
    case 0x470: return "SET_PICTURE_LUMA_OFFSET16";
    case 0x474: return "SET_PICTURE_CHROMA_OFFSET0";
    case 0x478: return "SET_PICTURE_CHROMA_OFFSET1";
    case 0x47c: return "SET_PICTURE_CHROMA_OFFSET2";
    case 0x480: return "SET_PICTURE_CHROMA_OFFSET3";
    case 0x484: return "SET_PICTURE_CHROMA_OFFSET4";
    case 0x488: return "SET_PICTURE_CHROMA_OFFSET5";
    case 0x48c: return "SET_PICTURE_CHROMA_OFFSET6";
    case 0x490: return "SET_PICTURE_CHROMA_OFFSET7";
    case 0x494: return "SET_PICTURE_CHROMA_OFFSET8";
    case 0x498: return "SET_PICTURE_CHROMA_OFFSET9";
    case 0x49c: return "SET_PICTURE_CHROMA_OFFSET10";
    case 0x4a0: return "SET_PICTURE_CHROMA_OFFSET11";
    case 0x4a4: return "SET_PICTURE_CHROMA_OFFSET12";
    case 0x4a8: return "SET_PICTURE_CHROMA_OFFSET13";
    case 0x4ac: return "SET_PICTURE_CHROMA_OFFSET14";
    case 0x4b0: return "SET_PICTURE_CHROMA_OFFSET15";
    case 0x4b4: return "SET_PICTURE_CHROMA_OFFSET16";
    case 0x4b8: return "SET_PIC_SCRATCH_BUF_OFFSET";
    case 0x4bc: return "SET_EXTERNAL_MVBUFFER_OFFSET";
    case 0x500: return "H264_SET_MBHIST_BUF_OFFSET";
    case 0x540: return "VP8_SET_PROB_DATA_OFFSET";
    case 0x544: return "VP8_SET_HEADER_PARTITION_BUF_BASE_OFFSET";
    case 0x580: return "HEVC_SET_SCALING_LIST_OFFSET";
    case 0x584: return "HEVC_SET_TILE_SIZES_OFFSET";
    case 0x588: return "HEVC_SET_FILTER_BUFFER_OFFSET";
    case 0x58c: return "HEVC_SET_SAO_BUFFER_OFFSET";
    case 0x590: return "HEVC_SET_SLICE_INFO_BUFFER_OFFSET";
    case 0x594: return "HEVC_SET_SLICE_GROUP_INDEX";
    case 0x5c0: return "VP9_SET_PROB_TAB_BUF_OFFSET";
    case 0x5c4: return "VP9_SET_CTX_COUNTER_BUF_OFFSET";
    case 0x5c8: return "VP9_SET_SEGMENT_READ_BUF_OFFSET";
    case 0x5cc: return "VP9_SET_SEGMENT_WRITE_BUF_OFFSET";
    case 0x5d0: return "VP9_SET_TILE_SIZE_BUF_OFFSET";
    case 0x5d4: return "VP9_SET_COL_MVWRITE_BUF_OFFSET";
    case 0x5d8: return "VP9_SET_COL_MVREAD_BUF_OFFSET";
    case 0x5dc: return "VP9_SET_FILTER_BUFFER_OFFSET";
    case 0x600: return "OS_INTERRUPT_HALT_ENGINE";
    case 0x700: return "OS_INTERRUPT_TRAP_NONSTALL";
    case 0x4c8: return "SET_INTRA_TOP_BUF_OFFSET(c9b0)";
    }
    return "";
}
static int is_dec_addr(NvU32 m)
{
    if (m < 0x404 || m >= 0x800 || m == 0x40c || m == 0x594) return 0;
    const char *n = nvdec_name(m);
    return !*n || strstr(n, "OFFSET") != NULL;
}

static void mem_desc(NvU64 va)
{
    Mem *m = mem_by_gpu(va);
    if (!m) { L("(no known memory)"); return; }
    L("mem 0x%08x cls 0x%x +0x%llx of 0x%llx type %u flags 0x%x attr 0x%08x attr2 0x%08x map 0x%x kind 0x%x%s",
      m->h, m->cls, (unsigned long long)(va - m->gpu), (unsigned long long)m->gpuLen, m->type, m->flags, m->attr, m->attr2,
      m->mapFlags, m->kind, m->cpu ? " cpu" : "");
}

// NVTRACE_VIDMEM: read a buffer the client never maps (video memory) by mapping it ourselves the way the library maps its
// own memory: open /dev/nvidia0, register that fd with the control fd (NV_ESC_REGISTER_FD), NV_ESC_RM_MAP_MEMORY, mmap
static int (*real_ioctl)(int, unsigned long, void *);
static void *(*real_mmap)(void *, size_t, int, int, int, off_t);
static void dump_unmapped(Mem *mm, NvU64 va, const char *dir, NvU32 m)
{
    NvU64 len = mm->size ? mm->size : mm->gpuLen; if (len > (16 << 20)) len = 16 << 20;
    int fd = open("/dev/nvidia0", O_RDWR | O_CLOEXEC); if (fd < 0) return;
    nv_ioctl_register_fd_t rf = { .ctl_fd = mm->ctlFd };
    if (real_ioctl(fd, _IOWR(NV_IOCTL_MAGIC, NV_ESC_REGISTER_FD, nv_ioctl_register_fd_t), &rf)) { L("    (vidmem 0x%08x: register fd failed)\n", mm->h); close(fd); return; }
    nv_ioctl_nvos33_parameters_with_fd mp = { .params = { .hClient = mm->client, .hDevice = mm->parent, .hMemory = mm->h, .offset = 0, .length = len }, .fd = fd };
    if (real_ioctl(mm->ctlFd, _IOWR(NV_IOCTL_MAGIC, NV_ESC_RM_MAP_MEMORY, nv_ioctl_nvos33_parameters_with_fd), &mp) || mp.params.status) {
        L("    (vidmem 0x%08x: map failed, status 0x%x)\n", mm->h, mp.params.status); close(fd); return; }
    void *p = real_mmap(NULL, len, PROT_READ, MAP_SHARED, fd, 0);
    if (p != MAP_FAILED) {
        NvU64 off = va - mm->gpu, n = len - off;
        char path[600]; snprintf(path, sizeof path, "%s/exec%d-%03x.bin", dir, gExecs, m);
        FILE *f = fopen(path, "wb"); if (f) { fwrite((NvU8 *)p + off, 1, n, f); fclose(f); L("    dumped %s (0x%llx bytes, video memory)\n", path, (unsigned long long)n); }
        ((int (*)(void *, size_t))dlsym(RTLD_NEXT, "munmap"))(p, len);   // not our hook: the poller holds gLock
    } else L("    (vidmem 0x%08x: mmap failed)\n", mm->h);
    NVOS34_PARAMETERS up = { .hClient = mm->client, .hDevice = mm->parent, .hMemory = mm->h, .pLinearAddress = mp.params.pLinearAddress };
    real_ioctl(mm->ctlFd, _IOWR(NV_IOCTL_MAGIC, NV_ESC_RM_UNMAP_MEMORY, NVOS34_PARAMETERS), &up);
    close(fd);
}

static void on_execute(Chan *c, NvU32 data)
{
    c->execs++; gExecs++;
    L("  ==== EXECUTE #%d (global #%d, 0x%x) on channel 0x%08x, %s state:\n", c->execs, gExecs, data, c->h, c->dec ? "NVDEC" : "NVENC");
    for (int i = 0; i < 0x1400 / 4; i++) {
        if (!c->set[i]) continue;
        NvU32 m = (NvU32)i * 4, v = c->state[i];
        L("    %03x %-30s 0x%08x", m, c->dec ? nvdec_name(m) : nvenc_name(m), v);
        if (c->dec ? is_dec_addr(m) : is_addr_method(m)) { L("  -> 0x%llx ", (unsigned long long)v << 8); mem_desc((NvU64)v << 8); }
        L("\n");
    }
    const NvU32 psm = c->dec ? 0x404 : 0x710;
    void *ps = c->set[psm / 4] ? va2cpu((NvU64)c->state[psm / 4] << 8) : NULL;
    if (ps) { L("    picture setup (4 KB, zero rows skipped):\n"); hexi("      ", ps, 0x1000); }
    else L("    picture setup not CPU-visible\n");
    const char *dir = getenv("NVTRACE_DIR");
    for (int i = 0; dir && i < 0x1400 / 4; i++) {
        NvU32 m = (NvU32)i * 4;
        if (!c->set[i] || !(c->dec ? is_dec_addr(m) : is_addr_method(m))) continue;
        // NVTRACE_ONLY=404,5c0: dump only these methods' buffers (keeps up with a client that runs ahead)
        if (getenv("NVTRACE_ONLY")) { char mm3[8]; snprintf(mm3, sizeof mm3, "%03x", m); if (!strstr(getenv("NVTRACE_ONLY"), mm3)) continue; }
        NvU64 va = (NvU64)c->state[i] << 8; Mem *mm = mem_by_gpu(va); void *b = va2cpu(va);
        if (mm && !b && getenv("NVTRACE_VIDMEM")) { dump_unmapped(mm, va, dir, m); continue; }
        if (!mm || !b) continue;
        NvU64 lim = mm->cpuLen ? mm->cpuLen : mm->gpuLen, n = lim - (va - mm->gpu); if (n > (16 << 20)) n = 16 << 20;
        char path[600]; snprintf(path, sizeof path, "%s/exec%d-%03x.bin", dir, gExecs, m);
        FILE *f = fopen(path, "wb"); if (f) { fwrite(b, 1, n, f); fclose(f); L("    dumped %s (0x%llx bytes)\n", path, (unsigned long long)n); }
    }
}

static void decode_push(Chan *c, const NvU32 *p, NvU32 n)
{
    for (NvU32 i = 0; i < n;) {
        NvU32 h = p[i++], op = h >> 29, sub = (h >> 13) & 7, cnt = (h >> 16) & 0x1fff, mthd = (h & 0xfff) << 2;
        if (op == NVC56F_DMA_SEC_OP_IMMD_DATA_METHOD) { cnt = 1; }
        else if (op != NVC56F_DMA_SEC_OP_INC_METHOD && op != NVC56F_DMA_SEC_OP_NON_INC_METHOD && op != 5) { L("    [%u] hdr 0x%08x (op %u)\n", i - 1, h, op); continue; }
        for (NvU32 k = 0; k < cnt && (op == 4 || i < n); k++) {
            NvU32 d = op == 4 ? (h >> 16) & 0x1fff : p[i++];
            NvU32 m = op == 1 ? mthd + 4 * k : op == 5 && k ? mthd + 4 : mthd;
            if (m == 0) c->subclass[sub] = d;
            int enc = (c->subclass[sub] & 0xff) == 0xb7, dec = (c->subclass[sub] & 0xff) == 0xb0;
            L("    sc%u %03x %-30s 0x%08x%s\n", sub, m, dec ? nvdec_name(m) : enc || m < 0x100 ? nvenc_name(m) : "", d, op == 4 ? " (immd)" : "");
            if (dec) c->dec = 1;
            if ((enc || dec) && m < 0x1400) { c->state[m / 4] = d; c->set[m / 4] = 1; if (m == 0x300) on_execute(c, d); }
        }
    }
}

// one pass over the followed GPFIFOs (caller holds gLock): from the poller thread, and before every RM ioctl and munmap,
// so a push is decoded before the library tears its buffers down
static void poll_once(void)
{
        for (int k = 0; k < gNChan; k++) {
            Chan *c = &gChan[k];
            gVaClient = c->client;
            volatile NvU64 *gp = va2cpu(c->gpfifoVa);
            if (!gp || !c->entries) continue;
            if (!c->snap) c->snap = calloc(c->entries, 8);
            for (int guard = 0; guard < 64; guard++) {
                NvU64 e = gp[c->cur];
                if (e == c->snap[c->cur]) break;
                c->snap[c->cur] = e;
                NvU32 e0 = (NvU32)e, e1 = (NvU32)(e >> 32);
                NvU64 va = (e0 & 0xfffffffcu) | ((NvU64)(e1 & 0xff) << 32); NvU32 len = (e1 >> 10) & 0x1fffff;
                L("  gpfifo[%u] chan 0x%08x: 0x%016llx -> pb 0x%llx, %u dwords\n", c->cur, c->h, (unsigned long long)e, (unsigned long long)va, len);
                const NvU32 *pb = va2cpu(va);
                if (pb && len) { NvU32 *copy = malloc(len * 4); memcpy(copy, pb, len * 4); decode_push(c, copy, len); free(copy); }
                else if (len) L("    (push buffer not CPU-visible)\n");
                c->cur = (c->cur + 1) % c->entries;
            }
        }
        gVaClient = 0;
}
static void *poller(void *arg)
{
    (void)arg;
    for (;;) { pthread_mutex_lock(&gLock); poll_once(); pthread_mutex_unlock(&gLock); usleep(20); }
    return NULL;
}
static void start_poller(void)
{
    static int started; if (started) return; started = 1;
    pthread_t t; pthread_create(&t, NULL, poller, NULL); pthread_detach(t);
}

// ---- escape decoding ----
static int gCurFd = -1;   // the fd of the ioctl being decoded
static void log_alloc(const char *who, NvHandle client, NvHandle parent, NvHandle obj, NvU32 cls, void *pp, NvU32 psz, NvU32 status)
{
    if (!status && (cls == 0x3e || cls == 0x40 || cls == 0x71)) { Mem *x = mem_get(client, obj, 1); x->parent = parent; x->ctlFd = gCurFd; }
    L("ALLOC %-24s client 0x%08x parent 0x%08x new 0x%08x class 0x%04x size %u -> 0x%x\n", who, client, parent, obj, cls, psz, status);
    if (((cls & 0xff) == 0xb7 || (cls & 0xff) == 0xb0) && !status) {   // NVENC / NVDEC object: follow its channel
        for (int i = gNAll - 1; i >= 0; i--) if (gAll[i].client == client && gAll[i].h == parent && gNChan < 16) {
            Chan *c = &gChan[gNChan++]; memset(c, 0, sizeof *c);
            c->client = client; c->h = parent; c->gpfifoVa = gAll[i].gpfifoVa; c->entries = gAll[i].entries;
            L("      -> NVENC object 0x%08x on channel 0x%08x: following its GPFIFO at 0x%llx (%u entries)\n", obj, parent,
              (unsigned long long)c->gpfifoVa, c->entries);
            start_poller();
            break;
        }
    }
    if (!pp) return;
    if (cls == 0x3e || cls == 0x40 || cls == 0x50a0 || cls == 0x41) {   // NV01_MEMORY_SYSTEM / LOCAL_USER / NV50_MEMORY_VIRTUAL
        NV_MEMORY_ALLOCATION_PARAMS *m = pp;
        L("      type %u flags 0x%08x attr 0x%08x attr2 0x%08x format %u size 0x%llx align 0x%llx offset 0x%llx hVASpace 0x%08x pitch %u w %u h %u\n",
          m->type, m->flags, m->attr, m->attr2, m->format, (unsigned long long)m->size, (unsigned long long)m->alignment,
          (unsigned long long)m->offset, m->hVASpace, m->pitch, m->width, m->height);
        if (!status) { Mem *x = mem_get(client, obj, 1); x->cls = cls; x->type = m->type; x->flags = m->flags; x->attr = m->attr; x->attr2 = m->attr2; x->size = m->size; }
    } else if (cls == 0x71) {                                           // NV01_MEMORY_SYSTEM_OS_DESCRIPTOR: user memory
        NV_OS_DESC_MEMORY_ALLOCATION_PARAMS *m = pp;
        L("      type %u flags 0x%08x attr 0x%08x attr2 0x%08x descriptor 0x%llx limit 0x%llx descriptorType %u\n", m->type, m->flags,
          m->attr, m->attr2, (unsigned long long)m->descriptor, (unsigned long long)m->limit, m->descriptorType);
        if (!status) {
            Mem *x = mem_get(client, obj, 1); x->cls = cls; x->type = m->type; x->flags = m->flags; x->attr = m->attr; x->attr2 = m->attr2;
            x->size = m->limit + 1; if (m->descriptorType == 0) { x->cpu = (void *)(uintptr_t)m->descriptor; x->cpuLen = x->size; }
        }
    } else if ((cls & 0xff) == 0x6f) {
        NV_CHANNEL_ALLOC_PARAMS *ch = pp;
        L("      gpFifoOffset 0x%llx entries %u flags 0x%x hObjectError 0x%08x hObjectBuffer 0x%08x hUserd 0x%08x userdOffset 0x%llx engineType 0x%x hVASpace 0x%08x\n",
          (unsigned long long)ch->gpFifoOffset, ch->gpFifoEntries, ch->flags, ch->hObjectError, ch->hObjectBuffer, ch->hUserdMemory[0],
          (unsigned long long)ch->userdOffset[0], ch->engineType, ch->hVASpace);
        // on Linux the engine comes from the channel group (engineType 0 here): remember every channel, follow it once an
        // NVENC object (class 0xXXb7) is created on it
        if (!status && gNAll < 256) gAll[gNAll++] = (ChanInfo){ client, obj, ch->gpFifoOffset, ch->gpFifoEntries };
    } else if (psz) {
        hexi("      ", pp, psz > 256 ? 256 : psz);
    }
}

static void esc_before(NvU32 cmd, void *d, size_t sz)
{
    if (cmd == NV_ESC_RM_UNMAP_MEMORY && sz >= 12) { const NvU32 *w = d; Mem *m = mem_get(w[0], w[2], 0); if (m) m->cpu = NULL; }
}

static void esc_after(NvU32 cmd, void *d, size_t sz, void *ra)
{
    const char *who = caller(ra);
    switch (cmd) {
    case NV_ESC_RM_ALLOC:
        if (sz >= sizeof(NVOS64_PARAMETERS)) { NVOS64_PARAMETERS *p = d; log_alloc(who, p->hRoot, p->hObjectParent, p->hObjectNew, p->hClass, (void *)(uintptr_t)p->pAllocParms, p->paramsSize, p->status); }
        else { NVOS21_PARAMETERS *p = d; log_alloc(who, p->hRoot, p->hObjectParent, p->hObjectNew, p->hClass, (void *)(uintptr_t)p->pAllocParms, p->paramsSize, p->status); }
        break;
    case NV_ESC_RM_CONTROL: {
        NVOS54_PARAMETERS *p = d;
        L("CTRL  %-24s client 0x%08x obj 0x%08x cmd 0x%08x size %u -> 0x%x\n", who, p->hClient, p->hObject, p->cmd, p->paramsSize, p->status);
        // NVTRACE_CTRLMAX=N: dump up to N bytes of each control's parameters (default 128; RM-sensor structs are larger)
        static unsigned cmax; if (!cmax) { const char *e = getenv("NVTRACE_CTRLMAX"); cmax = e && atoi(e) > 0 ? (unsigned)atoi(e) : 128; }
        if (p->params && p->paramsSize) hexi("      ", (void *)(uintptr_t)p->params, p->paramsSize > cmax ? cmax : p->paramsSize);
        break; }
    case NV_ESC_RM_FREE: {
        NVOS00_PARAMETERS *p = d;
        L("FREE  %-24s client 0x%08x parent 0x%08x obj 0x%08x -> 0x%x\n", who, p->hRoot, p->hObjectParent, p->hObjectOld, p->status);
        break; }
    case NV_ESC_RM_MAP_MEMORY_DMA: {
        NVOS46_PARAMETERS *p = d;
        L("MAPDMA %-23s client 0x%08x dma 0x%08x mem 0x%08x off 0x%llx len 0x%llx flags 0x%08x flags2 0x%x kind 0x%x -> gpu 0x%llx status 0x%x\n",
          who, p->hClient, p->hDma, p->hMemory, (unsigned long long)p->offset, (unsigned long long)p->length, p->flags, p->flags2,
          p->kindOverride, (unsigned long long)p->dmaOffset, p->status);
        if (!p->status) { Mem *m = mem_get(p->hClient, p->hMemory, 1); m->gpu = p->dmaOffset - p->offset; m->gpuLen = p->offset + p->length; m->mapFlags = p->flags; m->kind = p->kindOverride; }
        break; }
    case NV_ESC_RM_MAP_MEMORY: {
        nv_ioctl_nvos33_parameters_with_fd *p = d;
        L("MAP   %-24s client 0x%08x dev 0x%08x mem 0x%08x off 0x%llx len 0x%llx fd %d -> flags 0x%08x status 0x%x\n", who, p->params.hClient,
          p->params.hDevice, p->params.hMemory, (unsigned long long)p->params.offset, (unsigned long long)p->params.length, p->fd, p->params.flags, p->params.status);
        if (!p->params.status && gNPend < 64) gPend[gNPend++] = (PendMap){ p->fd, p->params.hClient, p->params.hMemory, p->params.offset, p->params.length };
        break; }
    case NV_ESC_RM_ALLOC_MEMORY: {
        nv_ioctl_nvos02_parameters_with_fd *p = d;
        L("ALLOCMEM %-21s client 0x%08x parent 0x%08x new 0x%08x class 0x%x flags 0x%08x limit 0x%llx fd %d -> status 0x%x\n", who,
          p->params.hRoot, p->params.hObjectParent, p->params.hObjectNew, p->params.hClass, p->params.flags,
          (unsigned long long)p->params.limit, p->fd, p->params.status);
        L("      pMemory 0x%llx\n", (unsigned long long)p->params.pMemory);
        if (!p->params.status) {
            Mem *m = mem_get(p->params.hRoot, p->params.hObjectNew, 1); m->parent = p->params.hObjectParent; m->ctlFd = gCurFd; m->cls = p->params.hClass; m->flags = p->params.flags; m->size = p->params.limit + 1;
            // pMemory is not the user address: the library maps the memory with the next mmap() on that fd
            if (gNPend < 64) gPend[gNPend++] = (PendMap){ p->fd, p->params.hRoot, p->params.hObjectNew, 0, p->params.limit + 1 };
        }
        break; }
    default:
        L("ESC 0x%02x %-22s size %zu\n", cmd, who, sz);
        hexi("      ", d, sz > 128 ? 128 : sz);
    }
}

// ---- interposers ----
static void init(void)
{
    if (real_ioctl) return;
    real_ioctl = dlsym(RTLD_NEXT, "ioctl"); real_mmap = dlsym(RTLD_NEXT, "mmap");
    const char *o = getenv("NVTRACE_OUT");
    if (o && !gOut) { char p[512]; snprintf(p, sizeof p, "%s.%d", o, (int)getpid()); gOut = fopen(p, "w"); }
}

int ioctl(int fd, unsigned long req, ...)
{
    va_list a; va_start(a, req); void *arg = va_arg(a, void *); va_end(a);
    init();
    const unsigned type = (req >> 8) & 0xff, nr = req & 0xff; const size_t sz = (req >> 16) & 0x3fff;
    if (type != NV_IOCTL_MAGIC || !gOut) return real_ioctl(fd, req, arg);
    pthread_mutex_lock(&gLock);
    poll_once();
    esc_before(nr, arg, sz);
    pthread_mutex_unlock(&gLock);
    int r = real_ioctl(fd, req, arg);
    pthread_mutex_lock(&gLock);
    gCurFd = fd;
    esc_after(nr, arg, sz, __builtin_return_address(0));
    pthread_mutex_unlock(&gLock);
    return r;
}

void *mmap(void *addr, size_t len, int prot, int flags, int fd, off_t off)
{
    init();
    void *p = real_mmap(addr, len, prot, flags, fd, off);
    if (!gOut || p == MAP_FAILED || fd < 0) return p;
    pthread_mutex_lock(&gLock);
    L("mmap  fd %d len 0x%zx off 0x%llx flags 0x%x addr %p -> %p\n", fd, len, (unsigned long long)off, flags, addr, p);
    for (int i = 0; i < gNMem; i++) if (gMem[i].pendCpu && (NvU8 *)gMem[i].pendCpu >= (NvU8 *)p && (NvU8 *)gMem[i].pendCpu < (NvU8 *)p + len) {
        gMem[i].cpu = gMem[i].pendCpu; gMem[i].cpuLen = gMem[i].size; gMem[i].pendCpu = NULL;
        L("MMAP  covers mem 0x%08x at %p\n", gMem[i].h, gMem[i].cpu);
    }
    for (int i = gNPend - 1; i >= 0; i--) if (gPend[i].fd == fd) {
        Mem *m = mem_get(gPend[i].client, gPend[i].mem, 1);
        m->cpu = (NvU8 *)p - gPend[i].off; m->cpuLen = gPend[i].off + gPend[i].len;
        L("MMAP  fd %d len 0x%zx -> %p for mem 0x%08x (+0x%llx)\n", fd, len, p, gPend[i].mem, (unsigned long long)gPend[i].off);
        gPend[i] = gPend[--gNPend];
        break;
    }
    pthread_mutex_unlock(&gLock);
    return p;
}
void *mmap64(void *addr, size_t len, int prot, int flags, int fd, off_t off) __attribute__((alias("mmap")));

// an unmapped range stops being CPU-visible (the poller must not read it)
static int (*real_munmap)(void *, size_t);
int munmap(void *addr, size_t len)
{
    if (!real_munmap) real_munmap = dlsym(RTLD_NEXT, "munmap");
    pthread_mutex_lock(&gLock);
    poll_once();
    for (int i = 0; i < gNMem; i++)
        if (gMem[i].cpu && (NvU8 *)gMem[i].cpu + (gMem[i].cpuLen ? gMem[i].cpuLen : 1) > (NvU8 *)addr && (NvU8 *)gMem[i].cpu < (NvU8 *)addr + len) gMem[i].cpu = NULL;
    int r = real_munmap(addr, len);
    pthread_mutex_unlock(&gLock);
    return r;
}
