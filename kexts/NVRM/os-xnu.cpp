/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#include "nv-xnu.h"
#include <sys/kauth.h>
#include <mach/mach_time.h>
#include <mach/semaphore.h>
#include <kern/task.h>
#include <kern/clock.h>
#include <sys/systm.h>
#include <sys/proc.h>
#include <sys/random.h>
#include <mach/vm_param.h>
#include <kern/locks.h>

#define NV_XNU_TRC do { static bool _t = false; if (!_t) { _t = true; kprintf("NVRM-xnu: >%s\n", __func__); } } while (0)

static inline void nvx_memset(void *d, int c, size_t n) { __asm__ __volatile__("rep stosb" : "+D"(d), "+c"(n) : "a"(c) : "memory"); }
static inline void nvx_memcpy(void *d, const void *s, size_t n) { __asm__ __volatile__("rep movsb" : "+D"(d), "+S"(s), "+c"(n) : : "memory"); }

extern "C" {

NvU64 os_page_size     = PAGE_SIZE;
NvU64 os_page_mask     = ~(NvU64)(PAGE_SIZE - 1);
NvU8  os_page_shift    = PAGE_SHIFT;
NvU64 os_max_page_size = PAGE_SIZE;
NV_STATUS NV_API_CALL os_alloc_mem(void **address, NvU64 size)
{ NV_XNU_TRC;
    *address = kern_os_malloc((size_t)size);
    if (*address == NULL) return NV_ERR_NO_MEMORY;
    nvx_memset(*address, 0, (size_t)size);
    return NV_OK;
}
void  NV_API_CALL os_free_mem(void *address) { NV_XNU_TRC; if (address) kern_os_free(address); }
void *NV_API_CALL os_mem_copy(void *dst, const void *src, NvU32 length) { static unsigned long n = 0; if (++n <= 4 || (n % 1000000) == 0) kprintf("NVRM-xnu: >os_mem_copy #%lu len=%u\n", n, length); nvx_memcpy(dst, src, length); return dst; }
NV_STATUS NV_API_CALL os_memcpy_from_user(void *dst, const void *src, NvU32 length) { NV_XNU_TRC; return copyin((user_addr_t)(uintptr_t)src, dst, length) ? NV_ERR_INVALID_ADDRESS : NV_OK; }
NV_STATUS NV_API_CALL os_memcpy_to_user(void *dst, const void *src, NvU32 length) { NV_XNU_TRC; return copyout(src, (user_addr_t)(uintptr_t)dst, length) ? NV_ERR_INVALID_ADDRESS : NV_OK; }
void *NV_API_CALL os_mem_set(void *dst, NvU8 c, NvU32 length)
{
    static unsigned long n = 0; n++;
    if (n <= 4 || (n % 1000000) == 0) kprintf("NVRM-xnu: >os_mem_set #%lu len=%u\n", n, length);
    nvx_memset(dst, c, length);
    return dst;
}
NvS32 NV_API_CALL os_mem_cmp(const NvU8 *a, const NvU8 *b, NvU32 length) { NV_XNU_TRC; return memcmp(a, b, length); }
NvU32 NV_API_CALL os_string_length(const char *s) { NV_XNU_TRC; return (NvU32)strlen(s); }
char *NV_API_CALL os_string_copy(char *dst, const char *src) { NV_XNU_TRC; char *d = dst; while ((*d++ = *src++) != 0) {} return dst; }
NvS32 NV_API_CALL os_string_compare(const char *a, const char *b) { NV_XNU_TRC; return strcmp(a, b); }
NvS32 NV_API_CALL os_snprintf(char *buf, NvU32 size, const char *fmt, ...)
{ NV_XNU_TRC;
    va_list ap; va_start(ap, fmt); int n = vsnprintf(buf, size, fmt, ap); va_end(ap); return n;
}
NvU32 NV_API_CALL os_get_cpu_count(void) { NV_XNU_TRC; return 4; }
NvU32 NV_API_CALL os_get_cpu_number(void) { NV_XNU_TRC; return 0; }

int NV_API_CALL nv_printf(NvU32 debuglevel, const char *printf_format, ...)
{ NV_XNU_TRC;
    char buf[512];
    va_list ap; va_start(ap, printf_format);
    vsnprintf(buf, sizeof buf, printf_format, ap);
    va_end(ap);
    buf[sizeof buf - 1] = 0;
    kprintf("%s", buf);
    IOLog("%s", buf);
    return 0;
}
NV_STATUS NV_API_CALL nv_log_error(nv_state_t *nv, NvU32 error_number, const char *format, va_list ap)
{ NV_XNU_TRC;
    char buf[512];
    vsnprintf(buf, sizeof buf, format, ap);
    buf[sizeof buf - 1] = 0;
    kprintf("NVRM: Xid %u: %s\n", error_number, buf);
    IOLog("NVRM: Xid %u: %s\n", error_number, buf);
    return NV_OK;
}
#include <kern/thread_call.h>
#define NV_XNU_LOG_SLOTS 256
#define NV_XNU_LOG_LEN   256
static struct { volatile int ready; char text[NV_XNU_LOG_LEN]; } nv_xnu_log_ring[NV_XNU_LOG_SLOTS];
static volatile unsigned nv_xnu_log_head, nv_xnu_log_tail, nv_xnu_log_dropped;
static volatile int nv_xnu_log_draining;
static thread_call_t nv_xnu_log_call;

static void nv_xnu_log_drain(thread_call_param_t, thread_call_param_t)
{
    if (__atomic_exchange_n(&nv_xnu_log_draining, 1, __ATOMIC_ACQUIRE)) return;
    for (;;) {
        unsigned t = nv_xnu_log_tail;
        auto *slot = &nv_xnu_log_ring[t & (NV_XNU_LOG_SLOTS - 1)];
        if (!__atomic_load_n(&slot->ready, __ATOMIC_ACQUIRE)) break;
        IOLog("%s", slot->text);
        __atomic_store_n(&slot->ready, 0, __ATOMIC_RELEASE);
        nv_xnu_log_tail = t + 1;
    }
    unsigned d = __atomic_exchange_n(&nv_xnu_log_dropped, 0u, __ATOMIC_RELAXED);
    if (d) IOLog("NVRM-xnu: out_string ring full -- %u RM line(s) dropped (serial still has them)\n", d);
    __atomic_store_n(&nv_xnu_log_draining, 0, __ATOMIC_RELEASE);
}

void nv_xnu_log_init(void)
{
    if (!nv_xnu_log_call) nv_xnu_log_call = thread_call_allocate(nv_xnu_log_drain, NULL);
}

void NV_API_CALL out_string(const char *str)
{
    if (!str) return;
    kprintf("%s", str);
    if (!nv_xnu_log_call) return;
    unsigned h = __atomic_load_n(&nv_xnu_log_head, __ATOMIC_RELAXED);
    for (;;) {
        if (__atomic_load_n(&nv_xnu_log_ring[h & (NV_XNU_LOG_SLOTS - 1)].ready, __ATOMIC_ACQUIRE)) {
            __atomic_fetch_add(&nv_xnu_log_dropped, 1u, __ATOMIC_RELAXED); thread_call_enter(nv_xnu_log_call); return;
        }
        if (__atomic_compare_exchange_n(&nv_xnu_log_head, &h, h + 1, false, __ATOMIC_ACQ_REL, __ATOMIC_RELAXED)) break;
    }
    auto *slot = &nv_xnu_log_ring[h & (NV_XNU_LOG_SLOTS - 1)];
    unsigned i = 0; for (; i < NV_XNU_LOG_LEN - 1 && str[i]; i++) slot->text[i] = str[i];
    slot->text[i] = 0;
    __atomic_store_n(&slot->ready, 1, __ATOMIC_RELEASE);
    thread_call_enter(nv_xnu_log_call);
}
void NV_API_CALL os_dbg_init(void) { NV_XNU_TRC;}
void NV_API_CALL os_dbg_set_level(NvU32 level) { NV_XNU_TRC;}
void NV_API_CALL os_dbg_breakpoint(void) { NV_XNU_TRC; kprintf("NVRM-xnu: os_dbg_breakpoint\n"); }

NV_STATUS NV_API_CALL os_alloc_mutex(void **pp) { NV_XNU_TRC; *pp = IOLockAlloc(); return *pp ? NV_OK : NV_ERR_NO_MEMORY; }
void      NV_API_CALL os_free_mutex(void *p) { NV_XNU_TRC; if (p) IOLockFree((IOLock *)p); }
NV_STATUS NV_API_CALL os_acquire_mutex(void *p) { NV_XNU_TRC; IOLockLock((IOLock *)p); return NV_OK; }
NV_STATUS NV_API_CALL os_cond_acquire_mutex(void *p) { NV_XNU_TRC; return IOLockTryLock((IOLock *)p) ? NV_OK : NV_ERR_TIMEOUT_RETRY; }
void      NV_API_CALL os_release_mutex(void *p) { NV_XNU_TRC; IOLockUnlock((IOLock *)p); }

struct nv_xnu_spin { IOSimpleLock *l; IOInterruptState s; };
NV_STATUS NV_API_CALL os_alloc_spinlock(void **pp)
{ NV_XNU_TRC;
    nv_xnu_spin *sp = (nv_xnu_spin *)kern_os_malloc(sizeof *sp);
    if (!sp) return NV_ERR_NO_MEMORY;
    sp->l = IOSimpleLockAlloc(); sp->s = 0;
    if (!sp->l) { kern_os_free(sp); return NV_ERR_NO_MEMORY; }
    *pp = sp; return NV_OK;
}
void  NV_API_CALL os_free_spinlock(void *p) { NV_XNU_TRC; nv_xnu_spin *sp = (nv_xnu_spin *)p; if (sp) { IOSimpleLockFree(sp->l); kern_os_free(sp); } }
NvU64 NV_API_CALL os_acquire_spinlock(void *p) { NV_XNU_TRC; nv_xnu_spin *sp = (nv_xnu_spin *)p; sp->s = IOSimpleLockLockDisableInterrupt(sp->l); return 0; }
void  NV_API_CALL os_release_spinlock(void *p, NvU64 old) { NV_XNU_TRC; nv_xnu_spin *sp = (nv_xnu_spin *)p; IOSimpleLockUnlockEnableInterrupt(sp->l, sp->s); }

void     *NV_API_CALL os_alloc_rwlock(void) { NV_XNU_TRC; return IORWLockAlloc(); }
void      NV_API_CALL os_free_rwlock(void *p) { NV_XNU_TRC; if (p) IORWLockFree((IORWLock *)p); }
NV_STATUS NV_API_CALL os_acquire_rwlock_read(void *p) { NV_XNU_TRC; IORWLockRead((IORWLock *)p); return NV_OK; }
NV_STATUS NV_API_CALL os_acquire_rwlock_write(void *p) { NV_XNU_TRC; IORWLockWrite((IORWLock *)p); return NV_OK; }
NV_STATUS NV_API_CALL os_cond_acquire_rwlock_read(void *p) { NV_XNU_TRC; return lck_rw_try_lock(IORWLockGetMachLock((IORWLock *)p), LCK_RW_TYPE_SHARED) ? NV_OK : NV_ERR_TIMEOUT_RETRY; }
NV_STATUS NV_API_CALL os_cond_acquire_rwlock_write(void *p) { NV_XNU_TRC; return lck_rw_try_lock(IORWLockGetMachLock((IORWLock *)p), LCK_RW_TYPE_EXCLUSIVE) ? NV_OK : NV_ERR_TIMEOUT_RETRY; }
void      NV_API_CALL os_release_rwlock_read(void *p) { NV_XNU_TRC; IORWLockUnlock((IORWLock *)p); }
void      NV_API_CALL os_release_rwlock_write(void *p) { NV_XNU_TRC; IORWLockUnlock((IORWLock *)p); }

void *NV_API_CALL os_alloc_semaphore(NvU32 initial)
{ NV_XNU_TRC;
    semaphore_t *s = (semaphore_t *)kern_os_malloc(sizeof *s);
    if (!s) return NULL;
    if (semaphore_create(kernel_task, s, SYNC_POLICY_FIFO, (int)initial) != KERN_SUCCESS) { kern_os_free(s); return NULL; }
    return s;
}
void      NV_API_CALL os_free_semaphore(void *p) { NV_XNU_TRC; semaphore_t *s = (semaphore_t *)p; if (s) { semaphore_destroy(kernel_task, *s); kern_os_free(s); } }
NV_STATUS NV_API_CALL os_acquire_semaphore(void *p) { NV_XNU_TRC; return semaphore_wait(*(semaphore_t *)p) == KERN_SUCCESS ? NV_OK : NV_ERR_GENERIC; }
NV_STATUS NV_API_CALL os_cond_acquire_semaphore(void *p) { NV_XNU_TRC; return semaphore_wait_noblock(*(semaphore_t *)p) == KERN_SUCCESS ? NV_OK : NV_ERR_TIMEOUT_RETRY; }
NV_STATUS NV_API_CALL os_release_semaphore(void *p) { NV_XNU_TRC; semaphore_signal(*(semaphore_t *)p); return NV_OK; }

NvBool NV_API_CALL os_semaphore_may_sleep(void) { NV_XNU_TRC; return NV_TRUE; }
volatile thread_t nv_xnu_isr_thread = NULL;
volatile int nv_xnu_in_isr = 0;
NvBool NV_API_CALL os_is_isr(void) { return (nv_xnu_isr_thread != NULL && nv_xnu_isr_thread == current_thread()) ? NV_TRUE : NV_FALSE; }
NvBool NV_API_CALL os_is_administrator(void) { NV_XNU_TRC; return NV_TRUE; }
NV_STATUS NV_API_CALL os_get_euid(NvU32 *pSecToken) { NV_XNU_TRC; if (!pSecToken) return NV_ERR_INVALID_ARGUMENT; *pSecToken = (NvU32)kauth_getuid(); return NV_OK; }

NvU64 NV_API_CALL os_get_monotonic_time_ns(void)    { NV_XNU_TRC; uint64_t ns; absolutetime_to_nanoseconds(mach_absolute_time(), &ns); return ns; }
NvU64 NV_API_CALL os_get_monotonic_time_ns_hr(void) { NV_XNU_TRC; uint64_t ns; absolutetime_to_nanoseconds(mach_absolute_time(), &ns); return ns; }
NV_STATUS NV_API_CALL os_get_system_time(NvU32 *sec, NvU32 *usec) { NV_XNU_TRC; clock_sec_t s; clock_usec_t us; clock_get_calendar_microtime(&s, &us); *sec = (NvU32)s; *usec = (NvU32)us; return NV_OK; }
NvBool NV_API_CALL nv_is_chassis_notebook(void)     { NV_XNU_TRC; return NV_FALSE; }
NvBool NV_API_CALL nv_acpi_is_battery_present(void) { NV_XNU_TRC; return NV_FALSE; }
NvBool NV_API_CALL nv_platform_supports_s0ix(void)  { NV_XNU_TRC; return NV_FALSE; }
NvBool NV_API_CALL os_is_nvswitch_present(void)     { NV_XNU_TRC; return NV_FALSE; }

NV_STATUS NV_API_CALL os_delay(NvU32 ms) { NV_XNU_TRC; IOSleep(ms); return NV_OK; }
NV_STATUS NV_API_CALL os_delay_us(NvU32 us) { NV_XNU_TRC; IODelay(us); return NV_OK; }
NvU64 NV_API_CALL os_get_cpu_frequency(void) { NV_XNU_TRC; return 0; }
NvU32 NV_API_CALL os_get_current_process(void) { NV_XNU_TRC; return (NvU32)proc_selfpid(); }
void  NV_API_CALL os_get_current_process_name(char *buf, NvU32 len) { NV_XNU_TRC; proc_selfname(buf, (int)len); }
NV_STATUS NV_API_CALL os_get_current_thread(NvU64 *id) { NV_XNU_TRC; *id = (NvU64)(uintptr_t)current_thread(); return NV_OK; }
void *NV_API_CALL os_get_pid_info(void) { NV_XNU_TRC; return NULL; }
void  NV_API_CALL os_put_pid_info(void *pid_info) { NV_XNU_TRC;}
NV_STATUS NV_API_CALL os_find_ns_pid(void *pid_info, NvU32 *ns_pid) { NV_XNU_TRC; return NV_ERR_NOT_SUPPORTED; }
NV_STATUS NV_API_CALL os_get_random_bytes(NvU8 *buf, NvU16 n) { NV_XNU_TRC; read_random(buf, n); return NV_OK; }

#define NV_XNU_MAX_PCI 256
static nv_xnu_pci_slot nv_xnu_slots[NV_XNU_MAX_PCI];
static int nv_xnu_nslots;
void nv_xnu_register_pci(IOPCIDevice *pci, NvU32 domain, NvU8 bus, NvU8 slot, NvU8 function)
{
    if (!pci) return;
    for (int i = 0; i < nv_xnu_nslots; i++) {
        nv_xnu_pci_slot *s = &nv_xnu_slots[i];
        if (s->pci == pci ||
            (s->domain == domain && s->bus == bus && s->slot == slot && s->function == function)) {
            return; // already registered
        }
    }
    if (nv_xnu_nslots < NV_XNU_MAX_PCI) { nv_xnu_slots[nv_xnu_nslots++] = { pci, domain, bus, slot, function }; }
}

// The RM enumerates the whole PCI tree through os_pci_init_handle to find the
// host bridge (FHB), root ports and P2P bridges for its chipset object. Only the
// GPU used to be registered here, so that enumeration came up empty and the RM
// logged "FHB/P2P/3DCTRL not found in cached bus topology" + "Unable to get PCI
// port handles". Walk every IOPCIDevice in the IORegistry and register them all
// so the RM sees the real topology.
void nv_xnu_register_all_pci(void)
{
    OSDictionary *match = IOService::serviceMatching("IOPCIDevice");
    if (!match) return;
    OSIterator *it = IOService::getMatchingServices(match);
    if (it) {
        OSObject *obj;
        while ((obj = it->getNextObject()) != NULL) {
            IOPCIDevice *pd = OSDynamicCast(IOPCIDevice, obj);
            if (pd) {
                nv_xnu_register_pci(pd, 0, pd->getBusNumber(), pd->getDeviceNumber(), pd->getFunctionNumber());
            }
        }
        it->release();
    }
    match->release();
}
void *NV_API_CALL os_pci_init_handle(NvU32 domain, NvU8 bus, NvU8 slot, NvU8 function, NvU16 *vendor, NvU16 *device)
{ NV_XNU_TRC;
    for (int i = 0; i < nv_xnu_nslots; i++) {
        nv_xnu_pci_slot *s = &nv_xnu_slots[i];
        if (s->domain == domain && s->bus == bus && s->slot == slot && s->function == function) {
            if (vendor) *vendor = s->pci->configRead16(0x00);
            if (device) *device = s->pci->configRead16(0x02);
            return s;
        }
    }
    return NULL;
}
#define NV_PCI_H(h) do { if (!(h)) { static bool _w; if (!_w) { _w = true; kprintf("NVRM-xnu: %s: NULL pci handle\n", __func__); } return NV_ERR_INVALID_ARGUMENT; } } while (0)
NV_STATUS NV_API_CALL os_pci_read_byte(void *h, NvU32 off, NvU8 *v)   { NV_XNU_TRC; NV_PCI_H(h); *v = ((nv_xnu_pci_slot *)h)->pci->configRead8(off);  return NV_OK; }
NV_STATUS NV_API_CALL os_pci_read_word(void *h, NvU32 off, NvU16 *v)  { NV_XNU_TRC; NV_PCI_H(h); *v = ((nv_xnu_pci_slot *)h)->pci->configRead16(off); return NV_OK; }
NV_STATUS NV_API_CALL os_pci_read_dword(void *h, NvU32 off, NvU32 *v) { NV_XNU_TRC; NV_PCI_H(h); *v = ((nv_xnu_pci_slot *)h)->pci->configRead32(off); return NV_OK; }
NV_STATUS NV_API_CALL os_pci_write_byte(void *h, NvU32 off, NvU8 v)   { NV_XNU_TRC; NV_PCI_H(h); ((nv_xnu_pci_slot *)h)->pci->configWrite8(off, v);  return NV_OK; }
NV_STATUS NV_API_CALL os_pci_write_word(void *h, NvU32 off, NvU16 v)  { NV_XNU_TRC; NV_PCI_H(h); ((nv_xnu_pci_slot *)h)->pci->configWrite16(off, v); return NV_OK; }
NV_STATUS NV_API_CALL os_pci_write_dword(void *h, NvU32 off, NvU32 v) { NV_XNU_TRC; NV_PCI_H(h); ((nv_xnu_pci_slot *)h)->pci->configWrite32(off, v); return NV_OK; }

struct nv_xnu_map { IOMemoryDescriptor *md; IOMemoryMap *map; void *va; NvU64 phys; NvU64 size; void *alias; };
extern "C" { void *nv_pm_alias(NvU64, NvU64, void **); void nv_pm_alias_drop(void *); bool nv_pm_is_gpu(NvU64, NvU64);
             void nv_pm_foreign(NvU64, NvU64, NvU32, void *); }
#define NV_XNU_MAX_MAPS 1024
static nv_xnu_map nv_xnu_maps[NV_XNU_MAX_MAPS];
static unsigned nv_xnu_maps_high;
void *NV_API_CALL os_map_kernel_space(NvU64 start, NvU64 size, NvU32 mode)
{
    IOOptionBits cache = (mode == NV_MEMORY_CACHED) ? kIOMapDefaultCache : (mode == NV_MEMORY_WRITECOMBINED ? kIOMapWriteCombineCache : kIOMapInhibitCache);
    kprintf("NVRM-xnu: >os_map_kernel_space start=0x%llx size=0x%llx mode=%u\n", start, size, mode);
    if (!nv_pm_is_gpu(start, size)) {
        void *cookie = NULL; void *ava = nv_pm_alias(start, size, &cookie);
        if (ava) {
            for (int i = 0; i < NV_XNU_MAX_MAPS; i++) if (nv_xnu_maps[i].va == NULL) { nv_xnu_maps[i] = { NULL, NULL, ava, start, size, cookie }; return ava; }
            return ava;
        }
        nv_pm_foreign(start, size, mode, __builtin_return_address(0));
    }
    IOMemoryDescriptor *md = IOMemoryDescriptor::withPhysicalAddress((IOPhysicalAddress)start, (IOByteCount)size, kIODirectionInOut);
    if (!md) { kprintf("NVRM-xnu:   withPhysicalAddress FAILED\n"); }
    IOMemoryMap *map = md ? md->map(cache) : NULL;
    if (md && !map) { kprintf("NVRM-xnu:   md->map(cache=0x%x) FAILED\n", (unsigned)cache); md->release(); md = NULL; }
    void *va = map ? (void *)map->getVirtualAddress() : NULL;
    if (!va) return NULL;
    for (int i = 0; i < NV_XNU_MAX_MAPS; i++) if (nv_xnu_maps[i].va == NULL) {
        nv_xnu_maps[i] = { md, map, va, start, size };
        if ((unsigned)i + 1 > nv_xnu_maps_high) nv_xnu_maps_high = (unsigned)i + 1;
        kprintf("NVRM-xnu:   mapped -> 0x%llx (slot %d, high water %u/%u)\n",
                (unsigned long long)(uintptr_t)va, i, nv_xnu_maps_high, (unsigned)NV_XNU_MAX_MAPS);
        return va;
    }
    {
        static bool warned;
        if (!warned) { warned = true;
            kprintf("NVRM-xnu: *** nv_xnu_maps FULL at %u entries -- leaking bookkeeping for "
                    "0x%llx (+0x%llx). The mapping is still VALID and returned; raise "
                    "NV_XNU_MAX_MAPS. ***\n", (unsigned)NV_XNU_MAX_MAPS,
                    (unsigned long long)start, (unsigned long long)size); }
    }
    return va;
}
NvU64 nv_xnu_phys_for_kernel_va(const void *va)
{
    for (int i = 0; i < NV_XNU_MAX_MAPS; i++) if (nv_xnu_maps[i].va && (const char *)va >= (const char *)nv_xnu_maps[i].va && (const char *)va < (const char *)nv_xnu_maps[i].va + nv_xnu_maps[i].size)
        return nv_xnu_maps[i].phys + ((const char *)va - (const char *)nv_xnu_maps[i].va);
    return 0;
}
void NV_API_CALL os_unmap_kernel_space(void *va, NvU64 size)
{ NV_XNU_TRC;
    for (int i = 0; i < NV_XNU_MAX_MAPS; i++) if (nv_xnu_maps[i].va == va) {
        if (nv_xnu_maps[i].alias) nv_pm_alias_drop(nv_xnu_maps[i].alias);
        else { nv_xnu_maps[i].map->release(); nv_xnu_maps[i].md->release(); }
        nv_xnu_maps[i] = { NULL, NULL, NULL, 0, 0, NULL }; return;
    }
}
int nv_xnu_kmap_overlap(NvU64 phys, NvU64 len)
{
    for (int i = 0; i < NV_XNU_MAX_MAPS; i++)
        if (nv_xnu_maps[i].va && !nv_xnu_maps[i].alias && phys < nv_xnu_maps[i].phys + nv_xnu_maps[i].size && nv_xnu_maps[i].phys < phys + len) return 1;
    return 0;
}
void  NV_API_CALL os_io_write_byte(NvU32 addr, NvU8 v)   { NV_XNU_TRC; __asm__ __volatile__("outb %0, %1" :: "a"(v), "d"((NvU16)addr)); }
void  NV_API_CALL os_io_write_word(NvU32 addr, NvU16 v)  { NV_XNU_TRC; __asm__ __volatile__("outw %0, %1" :: "a"(v), "d"((NvU16)addr)); }
void  NV_API_CALL os_io_write_dword(NvU32 addr, NvU32 v) { NV_XNU_TRC; __asm__ __volatile__("outl %0, %1" :: "a"(v), "d"((NvU16)addr)); }
NvU8  NV_API_CALL os_io_read_byte(NvU32 addr)  { NV_XNU_TRC; NvU8 v;  __asm__ __volatile__("inb %1, %0" : "=a"(v) : "d"((NvU16)addr)); return v; }
NvU16 NV_API_CALL os_io_read_word(NvU32 addr)  { NV_XNU_TRC; NvU16 v; __asm__ __volatile__("inw %1, %0" : "=a"(v) : "d"((NvU16)addr)); return v; }
NvU32 NV_API_CALL os_io_read_dword(NvU32 addr) { NV_XNU_TRC; NvU32 v; __asm__ __volatile__("inl %1, %0" : "=a"(v) : "d"((NvU16)addr)); return v; }

NV_STATUS NV_API_CALL os_registry_init(void) { NV_XNU_TRC; return NV_OK; }
NvBool NV_API_CALL os_is_vgx_hyper(void) { NV_XNU_TRC; return NV_FALSE; }
NvBool NV_API_CALL os_is_grid_supported(void) { NV_XNU_TRC; return NV_FALSE; }
NvBool NV_API_CALL os_is_efi_enabled(void) { NV_XNU_TRC; return NV_TRUE; }
NV_STATUS NV_API_CALL os_get_is_openrm(NvBool *b) { NV_XNU_TRC; *b = NV_TRUE; return NV_OK; }
void   NV_API_CALL os_disable_console_access(void) { NV_XNU_TRC;}
void   NV_API_CALL os_enable_console_access(void) { NV_XNU_TRC;}
NV_STATUS NV_API_CALL os_flush_cpu_cache_all(void) { NV_XNU_TRC; return NV_OK; }
NV_STATUS NV_API_CALL os_flush_user_cache(void) { NV_XNU_TRC; return NV_OK; }
void   NV_API_CALL os_flush_cpu_write_combine_buffer(void) { NV_XNU_TRC; __asm__ __volatile__("sfence" ::: "memory"); }

}
