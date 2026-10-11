/*
 * kernel_fpu_begin/end for the macOS port (replaces LinuxKPI's linux_fpu.c, whose FreeBSD form calls
 * fpu_kern_enter, which xnu does not have). The display code computes clocks and bandwidth in floating point
 * inside these sections.
 *
 * xnu keeps a thread's FPU/SSE/AVX registers live while it owns them and marks a CPU whose registers belong to
 * nobody with CR0.TS (the next user access traps and loads the thread's saved state). So on entry, with interrupts
 * off for the whole section so nothing else runs on this CPU:
 *   - CR0.TS set: the registers hold nothing anyone will read back; clear TS, use them, and set TS again on exit;
 *   - CR0.TS clear: the registers are the interrupted thread's; XSAVE them (FXSAVE on CPUs without XSAVE) into this
 *     CPU's save area and restore them on exit.
 * In between, MXCSR and the x87 control word are reset to their defaults, as Linux does, so the caller's rounding
 * and flush-to-zero modes never leak into the driver's arithmetic.
 *
 * Sections nest (the AMD driver relies on it, as FreeBSD notes); only the outermost one saves and restores.
 */
#include <sys/types.h>
#include <sys/param.h>
#include <sys/systm.h>
#include <sys/malloc.h>
#include <sys/kernel.h>
#include <sys/proc.h>
#include <sys/smp.h>

#include <linux/compat.h>
#include <linux/sched.h>

#include <asm/fpu/api.h>

MALLOC_DEFINE(M_NMXFPU, "nmxfpu", "kernel FPU save areas");

#define CR0_TS 0x00000008UL
#define CPUID1_ECX_XSAVE (1u << 26)
#define CPUID1_ECX_OSXSAVE (1u << 27)
#define MXCSR_DEFAULT 0x1f80u

struct fpu_area {
    void *buf;                  /* 64-byte aligned save area */
    int ts_was_set;
    int saved;
};

static struct fpu_area *areas;  /* one per CPU, indexed by curcpu */
static void *area_mem;
static size_t area_size;        /* bytes per area, a multiple of 64 */
static size_t area_mem_size;
static int use_xsave;

static inline unsigned long read_cr0(void)
{
    unsigned long v;
    __asm__ __volatile__("mov %%cr0, %0" : "=r"(v));
    return v;
}

static inline void write_cr0(unsigned long v)
{
    __asm__ __volatile__("mov %0, %%cr0" : : "r"(v) : "memory");
}

static inline void fpu_cpuid(uint32_t leaf, uint32_t sub, uint32_t r[4])
{
    __asm__ __volatile__("cpuid" : "=a"(r[0]), "=b"(r[1]), "=c"(r[2]), "=d"(r[3]) : "a"(leaf), "c"(sub));
}

static void nmx_fpu_init(void *arg)
{
    uint32_t r[4];
    fpu_cpuid(1, 0, r);
    use_xsave = (r[2] & CPUID1_ECX_XSAVE) && (r[2] & CPUID1_ECX_OSXSAVE);
    size_t sz = 512;            /* the FXSAVE image */
    if (use_xsave) {
        fpu_cpuid(0xd, 0, r);
        sz = r[1];              /* EBX: bytes for the features XCR0 enables now */
    }
    area_size = roundup2(sz, 64);
    int ncpu = mp_maxid + 1;
    area_mem_size = area_size * (size_t)ncpu + 64;
    area_mem = malloc(area_mem_size, M_NMXFPU, M_WAITOK | M_ZERO);
    areas = malloc(sizeof(*areas) * (size_t)ncpu, M_NMXFPU, M_WAITOK | M_ZERO);
    if (area_mem == NULL || areas == NULL) {
        printf("NullMoth xkpi: no memory for kernel FPU save areas; floating-point sections will fail\n");
        return;
    }
    uintptr_t base = roundup2((uintptr_t)area_mem, 64);
    for (int i = 0; i < ncpu; i++)
        areas[i].buf = (void *)(base + area_size * (size_t)i);
}
SYSINIT(nmx_fpu, SI_SUB_DRIVERS, SI_ORDER_FIRST, nmx_fpu_init, NULL);

static void nmx_fpu_uninit(void *arg)
{
    free(areas, M_NMXFPU);
    free(area_mem, M_NMXFPU);
    areas = NULL;
    area_mem = NULL;
}
SYSUNINIT(nmx_fpu, SI_SUB_DRIVERS, SI_ORDER_FIRST, nmx_fpu_uninit, NULL);

static void fpu_enter(void)
{
    critical_enter();           /* interrupts off: this CPU runs nothing else until fpu_leave */
    if (areas == NULL)
        panic("NullMoth xkpi: kernel FPU section without save areas");
    struct fpu_area *a = &areas[curcpu];
    unsigned long cr0 = read_cr0();
    a->ts_was_set = (cr0 & CR0_TS) != 0;
    a->saved = 0;
    if (a->ts_was_set) {
        write_cr0(cr0 & ~CR0_TS);
    } else if (use_xsave) {
        __asm__ __volatile__("xsave64 (%0)" : : "r"(a->buf), "a"(0xffffffffu), "d"(0xffffffffu) : "memory");
        a->saved = 1;
    } else {
        __asm__ __volatile__("fxsave64 (%0)" : : "r"(a->buf) : "memory");
        a->saved = 1;
    }
    uint32_t mxcsr = MXCSR_DEFAULT;
    __asm__ __volatile__("fninit; ldmxcsr %0" : : "m"(mxcsr));
}

static void fpu_leave(void)
{
    struct fpu_area *a = &areas[curcpu];
    if (a->saved) {
        if (use_xsave)
            __asm__ __volatile__("xrstor64 (%0)" : : "r"(a->buf), "a"(0xffffffffu), "d"(0xffffffffu) : "memory");
        else
            __asm__ __volatile__("fxrstor64 (%0)" : : "r"(a->buf) : "memory");
    } else if (a->ts_was_set) {
        write_cr0(read_cr0() | CR0_TS);
    }
    critical_exit();
}

void lkpi_kernel_fpu_begin(void)
{
    if ((current->fpu_ctx_level)++ == 0)
        fpu_enter();
}

void lkpi_kernel_fpu_end(void)
{
    if (--(current->fpu_ctx_level) == 0)
        fpu_leave();
}

/* runs func outside any FPU section the caller is in (it may sleep), then re-enters at the same depth */
void lkpi_fpu_safe_exec(fpu_safe_exec_cb_t func, void *ctx)
{
    unsigned int save_fpu_level = __current_unallocated(curthread) ? 0 : current->fpu_ctx_level;
    if (__predict_false(save_fpu_level != 0)) {
        current->fpu_ctx_level = 1;
        kernel_fpu_end();
    }
    func(ctx);
    if (__predict_false(save_fpu_level != 0)) {
        kernel_fpu_begin();
        current->fpu_ctx_level = save_fpu_level;
    }
}
