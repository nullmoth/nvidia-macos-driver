// rm.c: see rm.h.
#include "rm.h"
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include "nv_escape.h"
#include "class/cl0005.h"
#include "class/cl0000.h"
#include "class/cl0080.h"
#include "ctrl/ctrl0080/ctrl0080gpu.h"
#include "class/cl2080.h"
#include "ctrl/ctrl0000/ctrl0000gpu.h"
#include "class/cl003e.h"
#include "class/cl0040.h"
#include "class/cl50a0.h"
#include "class/cl90f1.h"
#include "nv-unix-nvos-params-wrappers.h"

enum { SEL_ESC = 1, SEL_BINDFD = 3, SEL_EVWAIT = 6 };

static io_connect_t open_conn_fd(NvU32 type, int *fdOut)
{
    // NVAccel's bridge first (0x4E56xxxx), then NVRM itself, as NVK's nvrm_xnu_open does
    io_connect_t conn = 0; kern_return_t kr = KERN_FAILURE;
    io_service_t svc = IOServiceGetMatchingService(kIOMainPortDefault, IOServiceMatching("NVAccel"));
    if (svc) { kr = IOServiceOpen(svc, mach_task_self(), 0x4E560000u | type, &conn); IOObjectRelease(svc); }
    if (kr != KERN_SUCCESS) {
        svc = IOServiceGetMatchingService(kIOMainPortDefault, IOServiceMatching("NVRM"));
        if (!svc) return 0;
        kr = IOServiceOpen(svc, mach_task_self(), type, &conn); IOObjectRelease(svc);
        if (kr != KERN_SUCCESS) return 0;
    }
    int fd = open("/dev/null", O_RDONLY | O_CLOEXEC);   // the user client keys its RM file on an fd number
    uint64_t in = (uint64_t)fd;
    IOConnectCallScalarMethod(conn, SEL_BINDFD, &in, 1, NULL, NULL);
    if (fdOut) *fdOut = fd;
    return conn;
}

int rm_conn_open(RmConn *c, NvU32 type) { c->fd = -1; c->conn = open_conn_fd(type, &c->fd); return c->conn ? 0 : -1; }
void rm_conn_close(RmConn *c) { if (c->conn) IOServiceClose(c->conn); if (c->fd >= 0) close(c->fd); c->conn = 0; c->fd = -1; }

int rm_esc(Rm *rm, io_connect_t conn, NvU32 cmd, void *data, size_t size)
{
    uint64_t in = cmd; size_t out = size;
    kern_return_t kr = IOConnectCallMethod(conn, SEL_ESC, &in, 1, data, size, NULL, NULL, data, &out);
    if (kr != KERN_SUCCESS && rm->verbose) fprintf(stderr, "rm: esc 0x%02x -> 0x%08x\n", cmd, kr);
    return kr == KERN_SUCCESS ? 0 : -1;
}

// our own handle range: RM generates handles from 0xcaf00000 for objects it creates inside our client (a channel
// gets an internal channel group), so client-chosen handles must stay out of that range
NvHandle rm_handle(Rm *rm) { return 0x4e560000u + rm->nextHandle++; }

NV_STATUS rm_alloc(Rm *rm, NvHandle parent, NvHandle obj, NvU32 cls, void *params)
{
    NVOS21_PARAMETERS p = { .hRoot = rm->client, .hObjectParent = parent, .hObjectNew = obj, .hClass = cls, .pAllocParms = params };
    if (rm_esc(rm, rm->ctl, NV_ESC_RM_ALLOC, &p, sizeof p)) return NV_ERR_GENERIC;
    if (p.status && rm->verbose) fprintf(stderr, "rm: alloc class 0x%04x under 0x%08x -> 0x%x\n", cls, parent, p.status);
    return p.status;
}

NV_STATUS rm_free(Rm *rm, NvHandle parent, NvHandle obj)
{
    NVOS00_PARAMETERS p = { .hRoot = rm->client, .hObjectParent = parent, .hObjectOld = obj };
    if (rm_esc(rm, rm->ctl, NV_ESC_RM_FREE, &p, sizeof p)) return NV_ERR_GENERIC;
    return p.status;
}

NV_STATUS rm_ctrl(Rm *rm, NvHandle obj, NvU32 cmd, void *params, NvU32 size)
{
    NVOS54_PARAMETERS p = { .hClient = rm->client, .hObject = obj, .cmd = cmd, .params = params, .paramsSize = size };
    if (rm_esc(rm, rm->ctl, NV_ESC_RM_CONTROL, &p, sizeof p)) return NV_ERR_GENERIC;
    if (p.status && rm->verbose) fprintf(stderr, "rm: control 0x%08x -> 0x%x\n", cmd, p.status);
    return p.status;
}

int rm_open(Rm *rm)
{
    bool v = rm->verbose; memset(rm, 0, sizeof *rm); rm->verbose = v;
    rm->ctl = open_conn_fd(0, &rm->ctlFd); rm->dev = open_conn_fd(1, &rm->devFd);
    if (!rm->ctl || !rm->dev) { fprintf(stderr, "rm: no NVAccel/NVRM user client\n"); return -1; }
    if (rm_esc(rm, rm->ctl, NV_ESC_CARD_INFO, &rm->card, sizeof rm->card) || !rm->card.valid) { fprintf(stderr, "rm: card info failed\n"); return -1; }
    NVOS21_PARAMETERS p = { .hClass = NV01_ROOT_CLIENT };
    if (rm_esc(rm, rm->ctl, NV_ESC_RM_ALLOC, &p, sizeof p) || p.status) { fprintf(stderr, "rm: client alloc failed\n"); return -1; }
    rm->client = p.hObjectNew;
    NV0000_CTRL_GPU_GET_ID_INFO_V2_PARAMS id = { .gpuId = rm->card.gpu_id };
    if (rm_ctrl(rm, rm->client, NV0000_CTRL_CMD_GPU_GET_ID_INFO_V2, &id, sizeof id)) return -1;
    rm->device = rm_handle(rm); rm->subdevice = rm_handle(rm);
    NV0080_ALLOC_PARAMETERS a0 = { .deviceId = id.deviceInstance, .hClientShare = rm->client };
    NV2080_ALLOC_PARAMETERS a2 = { .subDeviceId = id.subDeviceInstance };
    if (rm_alloc(rm, rm->client, rm->device, NV01_DEVICE_0, &a0) || rm_alloc(rm, rm->device, rm->subdevice, NV20_SUBDEVICE_0, &a2)) return -1;
    return 0;
}

void rm_close(Rm *rm)
{
    if (rm->client) { NVOS00_PARAMETERS p = { .hRoot = rm->client, .hObjectParent = rm->client, .hObjectOld = rm->client }; rm_esc(rm, rm->ctl, NV_ESC_RM_FREE, &p, sizeof p); }
    if (rm->dev) IOServiceClose(rm->dev);
    if (rm->ctl) IOServiceClose(rm->ctl);
    if (rm->devFd > 0) close(rm->devFd);
    if (rm->ctlFd > 0) close(rm->ctlFd);
    rm->dev = rm->ctl = 0; rm->devFd = rm->ctlFd = 0; rm->client = 0;
}

NV_STATUS rm_va_init(Rm *rm)
{
    // same parameters as NVK on this driver: page tables may fall back to system memory, VA starts at 8 GB
    NV_VASPACE_ALLOCATION_PARAMETERS p = { .flags = NV_VASPACE_ALLOCATION_FLAGS_RETRY_PTE_ALLOC_IN_SYS, .vaBase = 0x200000000ull };
    rm->vaspace = rm_handle(rm);
    return rm_alloc(rm, rm->device, rm->vaspace, FERMI_VASPACE_A, &p);
}

NV_STATUS rm_map_cpu(Rm *rm, NvHandle parent, NvHandle obj, NvU64 size, bool sysmem, RmConn *conn, void **cpu)
{
    // macOS: RM_MAP_MEMORY on the control connection names a second connection (by its fd number), whose memory
    // type 0 then carries the mapping (as NVK's nvRmApiMapMemory). Video memory goes through the device node.
    *cpu = NULL;
    if (rm_conn_open(conn, sysmem ? 0 : 1)) return NV_ERR_GENERIC;
    nv_ioctl_nvos33_parameters_with_fd p = { .params = { .hClient = rm->client, .hDevice = parent, .hMemory = obj,
                                                         .offset = 0, .length = size }, .fd = conn->fd };
    if (rm_esc(rm, rm->ctl, NV_ESC_RM_MAP_MEMORY, &p, sizeof p) || p.params.status) {
        if (rm->verbose) fprintf(stderr, "rm: map 0x%08x -> 0x%x\n", obj, p.params.status);
        rm_conn_close(conn); return p.params.status ? p.params.status : NV_ERR_GENERIC;
    }
    NvU32 caching = DRF_VAL(OS33, _FLAGS, _CACHING_TYPE, p.params.flags);
    bool wc = !sysmem && (caching == NVOS33_FLAGS_CACHING_TYPE_WRITECOMBINED || getenv("RM_FORCE_WC"));
    if (rm->verbose || getenv("RM_SHOW_CACHING")) fprintf(stderr, "rm: CPU map 0x%08x %s: RM caching type %u -> %s\n", obj, sysmem ? "sysmem" : "vidmem", caching, wc ? "write-combined" : "default");
    mach_vm_address_t addr = 0; mach_vm_size_t len = 0;
    IOOptionBits cache = wc ? kIOMapWriteCombineCache : 0;
    if (getenv("RM_MAP_CACHE") && !sysmem) cache = (IOOptionBits)strtoul(getenv("RM_MAP_CACHE"), NULL, 0) << 8;   // research knob (BAR1 caching experiments): 1 inhibit, 2 writethru, 3 copyback, 4 write-combine
    kern_return_t kr = IOConnectMapMemory64(conn->conn, 0, mach_task_self(), &addr, &len, kIOMapAnywhere | cache);
    if (kr != KERN_SUCCESS) { if (rm->verbose) fprintf(stderr, "rm: IOConnectMapMemory64 -> 0x%08x\n", kr); rm_conn_close(conn); return NV_ERR_GENERIC; }
    *cpu = (void *)addr;
    conn->addr = (void *)addr; conn->len = len; conn->stub = p.params.pLinearAddress; conn->hParent = parent; conn->hMem = obj;
    return NV_OK;
}

void rm_unmap_cpu(Rm *rm, RmConn *conn)
{
    if (conn->addr) {
        IOConnectUnmapMemory64(conn->conn, 0, mach_task_self(), (mach_vm_address_t)(uintptr_t)conn->addr);
        NVOS34_PARAMETERS p = { .hClient = rm->client, .hDevice = conn->hParent, .hMemory = conn->hMem, .pLinearAddress = conn->stub };
        if ((rm_esc(rm, rm->ctl, NV_ESC_RM_UNMAP_MEMORY, &p, sizeof p) || p.status) && rm->verbose)
            fprintf(stderr, "rm: unmap 0x%08x -> 0x%x\n", conn->hMem, p.status);
        conn->addr = NULL;
    }
    rm_conn_close(conn);
}

NV_STATUS rm_mem_alloc(Rm *rm, RmMem *m, NvU64 size, NvU64 align, bool sysmem, bool cpuMap, bool gpuMap)
{
    memset(m, 0, sizeof *m); m->map.fd = -1;
    size = (size + 0xfff) & ~0xfffull; m->size = size; m->sysmem = sysmem;
    NV_MEMORY_ALLOCATION_PARAMS p = { .owner = rm->client, .type = NVOS32_TYPE_IMAGE,
        .flags = align ? NVOS32_ALLOC_FLAGS_ALIGNMENT_FORCE : 0, .attr = DRF_DEF(OS32, _ATTR, _PAGE_SIZE, _4KB),
        .size = size, .alignment = align };
    if (sysmem) p.attr |= DRF_DEF(OS32, _ATTR, _LOCATION, _PCI) | DRF_DEF(OS32, _ATTR, _COHERENCY, _CACHED);
    else { p.attr |= DRF_DEF(OS32, _ATTR, _LOCATION, _VIDMEM) | DRF_DEF(OS32, _ATTR, _COHERENCY, _UNCACHED); p.flags |= NVOS32_ALLOC_FLAGS_PERSISTENT_VIDMEM; }
    m->hPhys = rm_handle(rm);
    NV_STATUS st = rm_alloc(rm, rm->device, m->hPhys, sysmem ? NV01_MEMORY_SYSTEM : NV01_MEMORY_LOCAL_USER, &p);
    if (st) { m->hPhys = 0; return st; }
    if (cpuMap && (st = rm_map_cpu(rm, rm->device, m->hPhys, size, sysmem, &m->map, &m->cpu))) return st;
    if (gpuMap) {
        NV_MEMORY_ALLOCATION_PARAMS v = { .owner = rm->client, .type = NVOS32_TYPE_IMAGE,
            .flags = NVOS32_ALLOC_FLAGS_VIRTUAL | (align ? NVOS32_ALLOC_FLAGS_ALIGNMENT_FORCE : 0),
            .size = size, .alignment = align, .hVASpace = rm->vaspace };
        m->hVirt = rm_handle(rm);
        if ((st = rm_alloc(rm, rm->device, m->hVirt, NV50_MEMORY_VIRTUAL, &v))) { m->hVirt = 0; return st; }
        NVOS46_PARAMETERS d = { .hClient = rm->client, .hDevice = rm->device, .hDma = m->hVirt, .hMemory = m->hPhys,
            .offset = 0, .length = size,
            .flags = DRF_DEF(OS46, _FLAGS, _PAGE_KIND, _VIRTUAL) |
                     (sysmem ? DRF_DEF(OS46, _FLAGS, _CACHE_SNOOP, _ENABLE) : DRF_DEF(OS46, _FLAGS, _CACHE_SNOOP, _DISABLE)) };
        if (rm_esc(rm, rm->ctl, NV_ESC_RM_MAP_MEMORY_DMA, &d, sizeof d) || d.status) {
            if (rm->verbose) fprintf(stderr, "rm: map dma -> 0x%x\n", d.status);
            return d.status ? d.status : NV_ERR_GENERIC;
        }
        // hDma is a virtual memory object, so RM returns the absolute GPU address in dmaOffset (== the object's offset
        // for a mapping at its start)
        m->gpu = d.dmaOffset;
        if (rm->verbose) fprintf(stderr, "rm: %s %llu KB at GPU 0x%llx (virtual object at 0x%llx)\n", sysmem ? "sysmem" : "vidmem",
                                 (unsigned long long)(size >> 10), (unsigned long long)d.dmaOffset, (unsigned long long)v.offset);
    }
    return NV_OK;
}

void rm_mem_free(Rm *rm, RmMem *m)
{
    if (m->map.conn) rm_unmap_cpu(rm, &m->map);
    if (m->hVirt) rm_free(rm, rm->device, m->hVirt);
    if (m->hPhys) rm_free(rm, rm->device, m->hPhys);
    memset(m, 0, sizeof *m);
}

NV_STATUS rm_event_open(Rm *rm, RmEvent *ev, NvU32 notifyIndex)
{
    memset(ev, 0, sizeof *ev); ev->conn.fd = -1;
    if (rm_conn_open(&ev->conn, 0)) return NV_ERR_GENERIC;
    nv_ioctl_alloc_os_event_t oe = { .hClient = rm->client, .hDevice = rm->device, .fd = (NvU32)ev->conn.fd };
    if (rm_esc(rm, ev->conn.conn, NV_ESC_ALLOC_OS_EVENT, &oe, sizeof oe) || oe.Status) {
        NV_STATUS st = oe.Status ? oe.Status : NV_ERR_GENERIC; rm_conn_close(&ev->conn); return st;
    }
    NV0005_ALLOC_PARAMETERS p = { .hParentClient = rm->client, .hSrcResource = rm->subdevice, .hClass = NV01_EVENT_OS_EVENT,
        .notifyIndex = notifyIndex | NV01_EVENT_NONSTALL_INTR | NV01_EVENT_WITHOUT_EVENT_DATA | NV01_EVENT_SUBDEVICE_SPECIFIC,
        .data = (NvP64)(uintptr_t)ev->conn.fd };
    ev->h = rm_handle(rm);
    NV_STATUS st = rm_alloc(rm, rm->subdevice, ev->h, NV01_EVENT_OS_EVENT, &p);
    if (st) { ev->h = 0; rm_conn_close(&ev->conn); }
    return st;
}

int64_t rm_event_wait(RmEvent *ev, uint32_t seen, uint32_t timeout_us)
{
    uint64_t in[2] = { seen, timeout_us }, out = 0; uint32_t n = 1;
    kern_return_t kr = IOConnectCallScalarMethod(ev->conn.conn, SEL_EVWAIT, in, 2, &out, &n);
    return kr == KERN_SUCCESS ? (int64_t)(uint32_t)out : -1;
}

void rm_event_close(Rm *rm, RmEvent *ev)
{
    if (ev->h) rm_free(rm, rm->subdevice, ev->h);
    if (ev->conn.conn) {
        nv_ioctl_free_os_event_t fe = { .hClient = rm->client, .hDevice = rm->device, .fd = (NvU32)ev->conn.fd };
        rm_esc(rm, ev->conn.conn, NV_ESC_FREE_OS_EVENT, &fe, sizeof fe);
        rm_conn_close(&ev->conn);
    }
    memset(ev, 0, sizeof *ev);
}

NvU32 rm_class(Rm *rm, int kind)
{
    if (!rm->clsRead) {
        static const NvU32 want[4][8] = {
            { 0xc9b7, 0xc7b7, 0xc5b7, 0xc4b7, 0xb4b7, 0xc3b7 },             // NVENC: Ada, Ampere, Turing, TU117 (Volta's), Volta
            { 0xc9b0, 0xc7b0, 0xc6b0, 0xc5b0, 0xc4b0, 0xc3b0 },             // NVDEC
            { 0xc56f, 0xc46f, 0xc36f },                                     // GPFIFO: Ampere, Turing, Volta
            { 0xc561, 0xc461, 0xc361 } };                                   // usermode (doorbell page)
        static NV0080_CTRL_GPU_GET_CLASSLIST_V2_PARAMS cl; memset(&cl, 0, sizeof cl);
        if (rm_ctrl(rm, rm->device, NV0080_CTRL_CMD_GPU_GET_CLASSLIST_V2, &cl, sizeof cl) == NV_OK)
            for (int k = 0; k < 4; k++)
                for (int w = 0; w < 8 && want[k][w] && !rm->cls[k]; w++)
                    for (NvU32 i = 0; i < cl.numClasses; i++) if (cl.classList[i] == want[k][w]) { rm->cls[k] = want[k][w]; break; }
        rm->clsRead = true;
        if (rm->verbose) fprintf(stderr, "rm: NVENC 0x%x NVDEC 0x%x channel 0x%x usermode 0x%x\n", rm->cls[0], rm->cls[1], rm->cls[2], rm->cls[3]);
    }
    return kind >= 0 && kind < 4 ? rm->cls[kind] : 0;
}
