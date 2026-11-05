// nvtrace.c: a DYLD_INSERT_LIBRARIES interposer that records how a process drives NVIDIA RM on nullmoth's driver,
// to compare a working NVENC client with ours (T2.05d, observation at the RM/IOKit boundary, no disassembly):
//   - every RM escape (NV_ESC_RM_ALLOC / CONTROL / FREE / MAP_MEMORY / MAP_MEMORY_DMA ...) with its parameters and
//     the calling image;
//   - memory objects: class, type/flags/attr/attr2, size, GPU address, CPU mapping;
//   - for channels on an NVENC engine: a thread follows the GPFIFO, decodes each pushed segment (methods per
//     subchannel), and at every NVENC EXECUTE dumps the method state, the memory behind each address, and the
//     picture setup buffer.
//   DYLD_INSERT_LIBRARIES=~/nvenc-dev/nvtrace.dylib NVTRACE_OUT=/tmp/t.log ./vttest hw avc1 256 128 3
#include <IOKit/IOKitLib.h>
#include <dlfcn.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include "nvtypes.h"
#include "nvos.h"
#include "nvstatus.h"
#include "nv-ioctl.h"
#include "nv_escape.h"
#include "nv-unix-nvos-params-wrappers.h"
#include "alloc/alloc_channel.h"
#include "class/cl2080_notification.h"
#include "class/clc56f.h"

#define SEL_ESC 1
#define SEL_BINDFD 3

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

// ---- connections: conn -> bound fd, pending CPU map ----
typedef struct { io_connect_t conn; int fd; NvU32 type; NvHandle mapClient, mapMem; NvU64 mapOff, mapLen; } Conn;
static Conn gConn[512]; static int gNConn;
static Conn *conn_get(io_connect_t c, int create)
{
    for (int i = 0; i < gNConn; i++) if (gConn[i].conn == c) return &gConn[i];
    if (!create || gNConn == 512) return NULL;
    memset(&gConn[gNConn], 0, sizeof gConn[0]); gConn[gNConn].conn = c; gConn[gNConn].fd = -1; return &gConn[gNConn++];
}
static Conn *conn_by_fd(int fd) { for (int i = gNConn - 1; i >= 0; i--) if (gConn[i].fd == fd) return &gConn[i]; return NULL; }

// ---- memory objects ----
typedef struct { NvHandle client, h; NvU32 cls, type, flags, attr, attr2, mapFlags, kind; NvU64 size; void *cpu; io_connect_t cpuConn; NvU64 gpu, gpuLen; } Mem;
static Mem gMem[8192]; static int gNMem;
static Mem *mem_get(NvHandle client, NvHandle h, int create)
{
    for (int i = gNMem - 1; i >= 0; i--) if (gMem[i].client == client && gMem[i].h == h) return &gMem[i];
    if (!create || gNMem == 8192) return NULL;
    memset(&gMem[gNMem], 0, sizeof gMem[0]); gMem[gNMem].client = client; gMem[gNMem].h = h; return &gMem[gNMem++];
}
// GPU addresses are per client (VA space): since driver 2.3.6 the encoder library and NVK in the same process each have
// a client, with GPFIFOs at the same address. The poller sets the client of the channel it follows.
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
    return m && m->cpu ? (NvU8 *)m->cpu + (va - m->gpu) : NULL;
}

// ---- NVENC channels ----
typedef struct {
    NvHandle client, h; NvU32 engineType; NvU64 gpfifoVa; NvU32 entries, cur; NvU64 *snap;
    NvU32 subclass[8]; NvU32 state[0x1400 / 4]; NvU8 set[0x1400 / 4]; int execs;
} Chan;
static Chan gChan[16]; static int gNChan;

static const char *nvenc_name(NvU32 m)
{
    switch (m) {
    case 0x000: return "SET_OBJECT"; case 0x100: return "NOP"; case 0x140: return "PM_TRIGGER"; case 0x200: return "SET_APPLICATION_ID";
    case 0x204: return "SET_WATCHDOG_TIMER"; case 0x240: return "SEMAPHORE_A"; case 0x244: return "SEMAPHORE_B"; case 0x248: return "SEMAPHORE_C";
    case 0x254: return "SET_SEMAPHORE_PAYLOAD_LOWER"; case 0x258: return "SET_SEMAPHORE_PAYLOAD_UPPER"; case 0x300: return "EXECUTE";
    case 0x304: return "SEMAPHORE_D"; case 0x4c8: return "SET_TOTAL_CORE_NUM"; case 0x700: return "SET_CONTROL_PARAMS";
    case 0x704: return "SET_PICTURE_INDEX"; case 0x708: return "SET_OUT_ENCRYPT_PARAMS"; case 0x70c: return "SET_IN_RCDATA";
    case 0x710: return "SET_IN_DRV_PIC_SETUP"; case 0x714: return "SET_IN_CEAHINTS_DATA"; case 0x718: return "SET_OUT_ENC_STATUS";
    case 0x71c: return "SET_OUT_BITSTREAM"; case 0x720: return "SET_IOHISTORY"; case 0x724: return "SET_IO_RC_PROCESS";
    case 0x728: return "SET_IN_COLOC_DATA"; case 0x72c: return "SET_OUT_COLOC_DATA"; case 0x730: return "SET_OUT_REF_PIC_LUMA";
    case 0x734: return "SET_IN_CUR_PIC"; case 0x738: return "SET_IN_MEPRED_DATA"; case 0x73c: return "SET_OUT_MEPRED_DATA";
    case 0x740: return "SET_IN_CUR_PIC_CHROMA_U"; case 0x744: return "SET_IN_CUR_PIC_CHROMA_V"; case 0x748: return "SET_IN_QP_MAP";
    case 0x74c: return "SET_OUT_REF_PIC_CHROMA"; case 0x750: return "SET_IN_PARTITION_BUF"; case 0x758: return "SET_IN_CUR_PIC_TASK_STATUS";
    case 0x75c: return "SET_IN_REF_PIC_TASK_STATUS"; case 0x760: return "SET_OUT_TASK_STATUS"; case 0x764: return "SET_IN_MV_HINTS_TASK_STATUS";
    }
    if (m >= 0x400 && m < 0x440) return "SET_IN_REF_PICn_LUMA";
    if (m >= 0x440 && m < 0x480) return "SET_IN_REF_PICn_CHROMA";
    return "";
}

static void mem_desc(NvU64 va)
{
    Mem *m = mem_by_gpu(va);
    if (!m) { L("(no known memory)"); return; }
    L("mem 0x%08x cls 0x%x +0x%llx of 0x%llx type %u flags 0x%x attr 0x%08x attr2 0x%08x map 0x%x kind 0x%x%s",
      m->h, m->cls, (unsigned long long)(va - m->gpu), (unsigned long long)m->gpuLen, m->type, m->flags, m->attr, m->attr2,
      m->mapFlags, m->kind, m->cpu ? " cpu" : "");
}

static void on_execute(Chan *c, NvU32 data)
{
    c->execs++;
    L("  ==== EXECUTE #%d (0x%x) on channel 0x%08x, NVENC state:\n", c->execs, data, c->h);
    for (int i = 0; i < 0x1400 / 4; i++) {
        if (!c->set[i]) continue;
        NvU32 m = (NvU32)i * 4, v = c->state[i];
        L("    %03x %-28s 0x%08x", m, nvenc_name(m), v);
        if ((m >= 0x400 && m < 0x4c8) || (m >= 0x708 && m < 0x780 && m != 0x704)) { L("  -> 0x%llx ", (unsigned long long)v << 8); mem_desc((NvU64)v << 8); }
        L("\n");
    }
    void *ps = c->set[0x710 / 4] ? va2cpu((NvU64)c->state[0x710 / 4] << 8) : NULL;
    if (ps) { L("    picture setup (32 KB, zero rows skipped):\n"); hexi("      ", ps, 0x8000); }
    else L("    picture setup not CPU-visible\n");
    // NVTRACE_DIR: raw dumps of the buffers behind each address method, <dir>/exec<N>-<method>.bin (whole memory object
    // from that address, at most 16 MB)
    const char *dir = getenv("NVTRACE_DIR");
    for (int i = 0; dir && i < 0x1400 / 4; i++) {
        NvU32 m = (NvU32)i * 4;
        if (!c->set[i] || !((m >= 0x400 && m < 0x4c8) || (m >= 0x708 && m < 0x780 && m != 0x704))) continue;
        NvU64 va = (NvU64)c->state[i] << 8; Mem *mm = mem_by_gpu(va); void *b = va2cpu(va);
        if (!mm || !b) continue;
        NvU64 n = mm->gpuLen - (va - mm->gpu); if (n > (16 << 20)) n = 16 << 20;
        char path[600]; snprintf(path, sizeof path, "%s/exec%d-%03x.bin", dir, c->execs, m);
        FILE *f = fopen(path, "wb"); if (f) { fwrite(b, 1, n, f); fclose(f); L("    dumped %s (0x%llx bytes)\n", path, (unsigned long long)n); }
    }
    static const struct { NvU32 m; const char *n; } more[] = { { 0x70c, "IN_RCDATA" }, { 0x724, "IO_RC_PROCESS" } };
    for (int i = 0; i < 2; i++) {
        void *b = c->set[more[i].m / 4] ? va2cpu((NvU64)c->state[more[i].m / 4] << 8) : NULL;
        if (b) { L("    %s (4 KB, zero rows skipped):\n", more[i].n); hexi("      ", b, 4096); }
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
            int enc = (c->subclass[sub] & 0xff) == 0xb7;
            L("    sc%u %03x %-28s 0x%08x%s\n", sub, m, enc || m < 0x100 ? nvenc_name(m) : "", d, op == 4 ? " (immd)" : "");
            if (enc && m < 0x1400) { c->state[m / 4] = d; c->set[m / 4] = 1; if (m == 0x300) on_execute(c, d); }
        }
    }
}

static void *poller(void *arg)
{
    (void)arg;
    for (;;) {
        pthread_mutex_lock(&gLock);
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
        pthread_mutex_unlock(&gLock);
        usleep(20);
    }
    return NULL;
}

static void start_poller(void)
{
    static int started; if (started) return; started = 1;
    pthread_t t; pthread_create(&t, NULL, poller, NULL); pthread_detach(t);
}

// ---- escape decoding ----
static void log_alloc(const char *who, NvHandle client, NvHandle parent, NvHandle obj, NvU32 cls, void *pp, NvU32 psz, NvU32 status)
{
    L("ALLOC %-24s client 0x%08x parent 0x%08x new 0x%08x class 0x%04x size %u -> 0x%x\n", who, client, parent, obj, cls, psz, status);
    if (!pp) return;
    if (!psz) psz = (cls & 0xff) == 0x6f ? (NvU32)sizeof(NV_CHANNEL_ALLOC_PARAMS) : (cls & 0xff) == 0xb7 ? 12 : 64;   // RM sizes by class
    if (cls == 0x3e || cls == 0x40 || cls == 0x50a0 || cls == 0x41) {   // NV01_MEMORY_SYSTEM / LOCAL_USER / NV50_MEMORY_VIRTUAL
        NV_MEMORY_ALLOCATION_PARAMS *m = pp;
        L("      type %u flags 0x%08x attr 0x%08x attr2 0x%08x format %u size 0x%llx align 0x%llx offset 0x%llx hVASpace 0x%08x pitch %u w %u h %u\n",
          m->type, m->flags, m->attr, m->attr2, m->format, (unsigned long long)m->size, (unsigned long long)m->alignment,
          (unsigned long long)m->offset, m->hVASpace, m->pitch, m->width, m->height);
        if (!status) { Mem *x = mem_get(client, obj, 1); x->cls = cls; x->type = m->type; x->flags = m->flags; x->attr = m->attr; x->attr2 = m->attr2; x->size = m->size; }
    } else if (cls == 0x71) {                                           // NV01_MEMORY_SYSTEM_OS_DESCRIPTOR
        NV_OS_DESC_MEMORY_ALLOCATION_PARAMS *m = pp;
        L("      type %u flags 0x%08x attr 0x%08x attr2 0x%08x descriptor 0x%llx limit 0x%llx descriptorType %u\n", m->type, m->flags,
          m->attr, m->attr2, (unsigned long long)m->descriptor, (unsigned long long)m->limit, m->descriptorType);
        if (!status) { Mem *x = mem_get(client, obj, 1); x->cls = cls; x->type = m->type; x->flags = m->flags; x->attr = m->attr; x->attr2 = m->attr2; x->size = m->limit + 1; }
    } else if ((cls & 0xff) == 0x6f) {
        NV_CHANNEL_ALLOC_PARAMS *ch = pp;
        L("      gpFifoOffset 0x%llx entries %u flags 0x%x hObjectError 0x%08x hObjectBuffer 0x%08x hUserd 0x%08x userdOffset 0x%llx engineType 0x%x hVASpace 0x%08x hContextShare 0x%08x\n",
          (unsigned long long)ch->gpFifoOffset, ch->gpFifoEntries, ch->flags, ch->hObjectError, ch->hObjectBuffer, ch->hUserdMemory[0],
          (unsigned long long)ch->userdOffset[0], ch->engineType, ch->hVASpace, ch->hContextShare);
        hexi("      ", pp, psz > 512 ? 512 : psz);
        NvU32 et = ch->engineType;
        if (!status && (et == NV2080_ENGINE_TYPE_NVENC0 || et == NV2080_ENGINE_TYPE_NVENC1 || et == NV2080_ENGINE_TYPE_NVENC2) && gNChan < 16) {
            Chan *c = &gChan[gNChan++]; memset(c, 0, sizeof *c);
            c->client = client; c->h = obj; c->engineType = et; c->gpfifoVa = ch->gpFifoOffset; c->entries = ch->gpFifoEntries;
            L("      -> following this NVENC channel's GPFIFO\n");
            start_poller();
        }
    } else if (psz) {
        hexi("      ", pp, psz > 256 ? 256 : psz);
    }
}

static void esc_before(Conn *cn, NvU32 cmd, void *d, size_t sz, void *ra)
{
    (void)cn; (void)ra; (void)sz;
    if (cmd == NV_ESC_RM_UNMAP_MEMORY && sz >= 12) {   // hClient, hDevice, hMemory: the poller must stop reading it
        const NvU32 *w = d; Mem *m = mem_get(w[0], w[2], 0);
        if (m) m->cpu = NULL;
    }
    if (cmd == NV_ESC_RM_MAP_MEMORY && sz >= sizeof(nv_ioctl_nvos33_parameters_with_fd)) {
        // remember which connection the next IOConnectMapMemory64 belongs to
        nv_ioctl_nvos33_parameters_with_fd *p = d; Conn *mc = conn_by_fd(p->fd);
        if (mc) { mc->mapClient = p->params.hClient; mc->mapMem = p->params.hMemory; mc->mapOff = p->params.offset; mc->mapLen = p->params.length; }
    }
}

static void esc_after(Conn *cn, NvU32 cmd, void *d, size_t sz, kern_return_t kr, void *ra)
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
        if (p->params && p->paramsSize) hexi("      ", (void *)(uintptr_t)p->params, p->paramsSize > 128 ? 128 : p->paramsSize);
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
        if (!p->status) {
            Mem *m = mem_get(p->hClient, p->hMemory, 1);
            m->gpu = p->dmaOffset - p->offset; m->gpuLen = p->offset + p->length; m->mapFlags = p->flags; m->kind = p->kindOverride;
        }
        break; }
    case NV_ESC_RM_MAP_MEMORY: {
        nv_ioctl_nvos33_parameters_with_fd *p = d;
        L("MAP   %-24s client 0x%08x dev 0x%08x mem 0x%08x off 0x%llx len 0x%llx fd %d -> flags 0x%08x status 0x%x\n", who, p->params.hClient,
          p->params.hDevice, p->params.hMemory, (unsigned long long)p->params.offset, (unsigned long long)p->params.length, p->fd, p->params.flags, p->params.status);
        break; }
    default:
        L("ESC 0x%02x %-22s size %zu -> kr 0x%x\n", cmd, who, sz, kr);
        hexi("      ", d, sz > 128 ? 128 : sz);
    }
    (void)cn;
}

// ---- interposers ----
static kern_return_t my_IOServiceOpen(io_service_t s, task_port_t t, uint32_t type, io_connect_t *c)
{
    kern_return_t kr = IOServiceOpen(s, t, type, c);
    io_name_t cls = ""; IOObjectGetClass(s, cls);
    if (kr == KERN_SUCCESS && ((type & 0xffff0000u) == 0x4e560000u || strstr(cls, "NVRM"))) {
        pthread_mutex_lock(&gLock); Conn *cn = conn_get(*c, 1); if (cn) cn->type = type;
        L("OPEN  %-24s type 0x%08x -> conn 0x%x\n", caller(__builtin_return_address(0)), type, *c); pthread_mutex_unlock(&gLock);
    }
    return kr;
}

static kern_return_t my_IOConnectCallScalarMethod(mach_port_t c, uint32_t sel, const uint64_t *in, uint32_t inCnt, uint64_t *out, uint32_t *outCnt)
{
    kern_return_t kr = IOConnectCallScalarMethod(c, sel, in, inCnt, out, outCnt);
    pthread_mutex_lock(&gLock);
    Conn *cn = conn_get(c, 0);
    if (cn && sel == SEL_BINDFD && inCnt >= 1) { cn->fd = (int)in[0]; L("BIND  conn 0x%x fd %d -> 0x%x\n", c, cn->fd, kr); }
    pthread_mutex_unlock(&gLock);
    return kr;
}

static kern_return_t my_IOConnectCallMethod(mach_port_t c, uint32_t sel, const uint64_t *in, uint32_t inCnt, const void *inS, size_t inSCnt,
                                            uint64_t *out, uint32_t *outCnt, void *outS, size_t *outSCnt)
{
    void *ra = __builtin_return_address(0);
    pthread_mutex_lock(&gLock);
    Conn *cn = conn_get(c, 0);
    int esc = cn && sel == SEL_ESC && inCnt >= 1;
    if (cn && sel == SEL_BINDFD && inCnt >= 1) cn->fd = (int)in[0];
    if (esc) esc_before(cn, (NvU32)in[0], (void *)inS, inSCnt, ra);
    pthread_mutex_unlock(&gLock);
    kern_return_t kr = IOConnectCallMethod(c, sel, in, inCnt, inS, inSCnt, out, outCnt, outS, outSCnt);
    if (esc) {
        pthread_mutex_lock(&gLock);
        esc_after(cn, (NvU32)in[0], outS ? outS : (void *)inS, outS && outSCnt ? *outSCnt : inSCnt, kr, ra);
        pthread_mutex_unlock(&gLock);
    } else if (cn) {
        pthread_mutex_lock(&gLock); L("CALL  %-24s conn 0x%x sel %u -> 0x%x\n", caller(ra), c, sel, kr); pthread_mutex_unlock(&gLock);
    }
    return kr;
}

static kern_return_t my_IOConnectCallStructMethod(mach_port_t c, uint32_t sel, const void *inS, size_t inSCnt, void *outS, size_t *outSCnt)
{
    pthread_mutex_lock(&gLock); Conn *cn = conn_get(c, 0);
    if (cn) L("CALLS %-24s conn 0x%x sel %u in %zu\n", caller(__builtin_return_address(0)), c, sel, inSCnt);
    pthread_mutex_unlock(&gLock);
    return IOConnectCallStructMethod(c, sel, inS, inSCnt, outS, outSCnt);
}

static kern_return_t my_IOConnectMapMemory64(io_connect_t c, uint32_t type, task_port_t t, mach_vm_address_t *addr, mach_vm_size_t *size, IOOptionBits opt)
{
    kern_return_t kr = IOConnectMapMemory64(c, type, t, addr, size, opt);
    pthread_mutex_lock(&gLock);
    Conn *cn = conn_get(c, 0);
    if (cn) {
        L("CPUMAP %-23s conn 0x%x type %u opt 0x%x -> 0x%llx len 0x%llx (mem 0x%08x off 0x%llx) kr 0x%x\n", caller(__builtin_return_address(0)), c, type, opt,
          kr ? 0ull : (unsigned long long)*addr, kr ? 0ull : (unsigned long long)*size, cn->mapMem, (unsigned long long)cn->mapOff, kr);
        if (!kr && cn->mapMem) { Mem *m = mem_get(cn->mapClient, cn->mapMem, 1); m->cpu = (void *)(uintptr_t)(*addr - cn->mapOff); m->cpuConn = c; }
    }
    pthread_mutex_unlock(&gLock);
    return kr;
}

// Since driver 2.3.6 a mapping is undone by closing its connection (nvrm_xnu_munmap -> IOServiceClose): forget the
// CPU address, or the poller reads unmapped memory
static void forget_conn_maps(io_connect_t c)
{
    for (int i = 0; i < gNMem; i++) if (gMem[i].cpu && gMem[i].cpuConn == c) { gMem[i].cpu = NULL; gMem[i].cpuConn = 0; }
}
static kern_return_t my_IOServiceClose(io_connect_t c)
{
    pthread_mutex_lock(&gLock);
    if (conn_get(c, 0)) { forget_conn_maps(c); L("CLOSE  conn 0x%x\n", c); }
    pthread_mutex_unlock(&gLock);
    return IOServiceClose(c);
}
static kern_return_t my_IOConnectUnmapMemory64(io_connect_t c, uint32_t type, task_port_t t, mach_vm_address_t addr)
{
    pthread_mutex_lock(&gLock);
    if (conn_get(c, 0)) { forget_conn_maps(c); L("CPUUNMAP conn 0x%x 0x%llx\n", c, (unsigned long long)addr); }
    pthread_mutex_unlock(&gLock);
    return IOConnectUnmapMemory64(c, type, t, addr);
}

// ---- VideoToolbox encoder registration: wrap the factory and dump the encoder object's vtable (M4, T4.03) ----
typedef int32_t (*VtFactory)(uint32_t, const void *, void **);
extern int32_t VTRegisterVideoEncoderWithInfo(uint32_t, const void *, void *) __attribute__((weak_import));
extern const void *CMBaseObjectGetVTable(const void *) __attribute__((weak_import));
static VtFactory gRealFactory;

static void dump_ptrs(const char *name, const void *p, int n)
{
    L("  %s @ %p:\n", name, p);
    if (!p) return;
    const uint64_t *w = p;
    for (int i = 0; i < n; i++) {
        Dl_info di; const char *sym = "";
        if (w[i] > 0x100000 && dladdr((void *)w[i], &di) && di.dli_sname && di.dli_saddr == (void *)w[i]) sym = di.dli_sname;
        else if (w[i] > 0x100000 && dladdr((void *)w[i], &di) && di.dli_fname) sym = strrchr(di.dli_fname, '/') ? strrchr(di.dli_fname, '/') + 1 : di.dli_fname;
        L("    [%2d] +0x%02x %#018llx %s\n", i, i * 8, (unsigned long long)w[i], sym);
    }
}

static int32_t wrap_factory(uint32_t codec, const void *alloc, void **out)
{
    int32_t st = gRealFactory(codec, alloc, out);
    pthread_mutex_lock(&gLock);
    L("VTFACTORY codec 0x%08x -> %d, object %p\n", codec, st, out ? *out : NULL);
    if (!st && out && *out && CMBaseObjectGetVTable) {
        const uint64_t *vt = CMBaseObjectGetVTable(*out);
        dump_ptrs("vtable", vt, 6);
        if (vt) { dump_ptrs("vtable[0] (reserved)", (void *)vt[0], 12); dump_ptrs("vtable[1] (base class)", (void *)vt[1], 12);
                  dump_ptrs("vtable[2] (encoder class)", (void *)vt[2], 28); dump_ptrs("vtable[3]", (void *)vt[3], 12); }
    }
    pthread_mutex_unlock(&gLock);
    return st;
}

static int32_t my_VTRegisterVideoEncoderWithInfo(uint32_t codec, const void *info, void *factory)
{
    pthread_mutex_lock(&gLock);
    Dl_info di; const char *sym = dladdr(factory, &di) && di.dli_sname ? di.dli_sname : "?";
    L("VTREGISTER codec 0x%08x factory %p (%s)\n", codec, factory, sym);
    pthread_mutex_unlock(&gLock);
    if (getenv("NVTRACE_VT") && !gRealFactory) { gRealFactory = (VtFactory)factory; factory = (void *)wrap_factory; }
    return VTRegisterVideoEncoderWithInfo(codec, info, factory);
}

__attribute__((constructor)) static void init(void)
{
    char path[512]; const char *o = getenv("NVTRACE_OUT");
    if (!o) { snprintf(path, sizeof path, "/tmp/nvtrace.%d.log", getpid()); o = path; }
    gOut = fopen(o, "w");
    L("nvtrace in %s (pid %d)\n", getprogname(), getpid());
}

#define INTERPOSE(r, o) __attribute__((used)) static struct { const void *rep, *orig; } _interpose_##o __attribute__((section("__DATA,__interpose"))) = { (const void *)r, (const void *)o }
INTERPOSE(my_IOServiceOpen, IOServiceOpen);
INTERPOSE(my_IOConnectCallScalarMethod, IOConnectCallScalarMethod);
INTERPOSE(my_IOConnectCallMethod, IOConnectCallMethod);
INTERPOSE(my_IOConnectCallStructMethod, IOConnectCallStructMethod);
INTERPOSE(my_IOConnectMapMemory64, IOConnectMapMemory64);
INTERPOSE(my_IOServiceClose, IOServiceClose);
INTERPOSE(my_IOConnectUnmapMemory64, IOConnectUnmapMemory64);
INTERPOSE(my_VTRegisterVideoEncoderWithInfo, VTRegisterVideoEncoderWithInfo);
