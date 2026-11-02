// rm.h: a minimal NVIDIA RM client for macOS on nullmoth's driver. It talks to GSP-RM through the NVAccel/NVRM user
// client (the escape interface NVK uses): IOServiceOpen type 0x4E560000|0 (control) / |1 (device), selector 3 binds
// an fd number, selector 1 = RM escape (NV_ESC_RM_ALLOC / CONTROL / FREE ...). Structures: NVIDIA
// open-gpu-kernel-modules (the RM version the driver runs).
#pragma once
#include <IOKit/IOKitLib.h>
#include <stdbool.h>
#include "nvtypes.h"
#include "nvos.h"
#include "nvstatus.h"
#include "nv-ioctl.h"

typedef struct {
    io_connect_t ctl, dev;            // control and device user clients
    int ctlFd, devFd;                 // the fd numbers they are bound to (closed by rm_close)
    NvHandle client, device, subdevice;
    NvHandle vaspace;                 // our GPU address space (rm_va_init)
    nv_ioctl_card_info_t card;
    NvU32 nextHandle;                 // handles for objects we allocate
    bool verbose;
    NvU32 cls[4]; bool clsRead;       // rm_class: the classes this GPU has, read once
} Rm;

int  rm_open(Rm *rm);                                                    // client + device + subdevice; 0 = ok
void rm_close(Rm *rm);
NvHandle rm_handle(Rm *rm);                                              // a fresh object handle
NV_STATUS rm_alloc(Rm *rm, NvHandle parent, NvHandle obj, NvU32 cls, void *params);
NV_STATUS rm_free(Rm *rm, NvHandle parent, NvHandle obj);
NV_STATUS rm_ctrl(Rm *rm, NvHandle obj, NvU32 cmd, void *params, NvU32 size);
// The newest class of each kind this GPU offers (NV0080_CTRL_CMD_GPU_GET_CLASSLIST_V2, read once): Ada / Ampere use
// NVENC 0xc9b7 / NVDEC 0xc9b0 / AMPERE_CHANNEL_GPFIFO_A / AMPERE_USERMODE_A; the GTX 1650 (TU117) has NVENC 0xb4b7, NVDEC
// 0xc4b0 and the Turing host classes. 0 = none.
enum { RM_CLS_NVENC, RM_CLS_NVDEC, RM_CLS_CHANNEL, RM_CLS_USERMODE };
NvU32 rm_class(Rm *rm, int kind);
int rm_esc(Rm *rm, io_connect_t conn, NvU32 cmd, void *data, size_t size);

// A user-client connection bound to an fd number: RM ties memory mappings to such an fd ("memFd").
typedef struct {
    io_connect_t conn; int fd;
    // the CPU mapping it carries (rm_map_cpu), released by rm_unmap_cpu
    void *addr; NvU64 len; NvP64 stub; NvHandle hParent, hMem;
} RmConn;
int rm_conn_open(RmConn *c, NvU32 type);                                  // type 0 = control node, 1 = device node
void rm_conn_close(RmConn *c);

// Memory: system (host, GPU-coherent) or video memory, optionally CPU-mapped and given a GPU address.
typedef struct {
    NvHandle hPhys, hVirt;
    NvU64 size;
    bool sysmem;
    void *cpu;                        // CPU mapping (NULL if none)
    NvU64 gpu;                        // GPU virtual address in rm->vaspace (0 if none)
    RmConn map;                       // the connection that carries the CPU mapping
} RmMem;
NV_STATUS rm_va_init(Rm *rm);                                            // FERMI_VASPACE_A for our objects
NV_STATUS rm_mem_alloc(Rm *rm, RmMem *m, NvU64 size, NvU64 align, bool sysmem, bool cpuMap, bool gpuMap);
void rm_mem_free(Rm *rm, RmMem *m);
// CPU-map any RM object that supports it (memory, usermode doorbell page) through a fresh connection
NV_STATUS rm_map_cpu(Rm *rm, NvHandle parent, NvHandle obj, NvU64 size, bool sysmem, RmConn *conn, void **cpu);
// undo rm_map_cpu: unmap from the task, RM UNMAP_MEMORY (NVK's order), close the connection
void rm_unmap_cpu(Rm *rm, RmConn *conn);

// An RM event on a non-stall interrupt notifier (e.g. NV2080_NOTIFIERS_NVENC0), waited on through the user client's
// selector 6 (as nullmoth's NVK port): a connection registered with NV_ESC_ALLOC_OS_EVENT carries the event count.
typedef struct { RmConn conn; NvHandle h; } RmEvent;
NV_STATUS rm_event_open(Rm *rm, RmEvent *ev, NvU32 notifyIndex);
int64_t rm_event_wait(RmEvent *ev, uint32_t seen, uint32_t timeout_us);   // returns the count (seen 0, timeout 0: just read it); < 0 = error
void rm_event_close(Rm *rm, RmEvent *ev);
