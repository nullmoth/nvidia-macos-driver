/*
 * FreeBSD <sys/systm.h> on xnu: xnu's (printf, panic, bzero, copyin/copyout, ...), plus what FreeBSD's systm.h brings
 * in that LinuxKPI relies on: assertions, CPU helpers, callouts, and the few kernel functions it declares.
 */
#ifndef NMXKPI_SYS_SYSTM_H
#define NMXKPI_SYS_SYSTM_H
#include_next <sys/systm.h>
/* xnu's systm.h defines SYSINIT, MALLOC_DEFINE and KASSERT as EMPTY macros; restore FreeBSD's */
#undef SYSINIT
#undef MALLOC_DEFINE
#undef KASSERT
#undef getenv_int
#define SYSINIT(u, s, o, f, i) NMX_SYSINIT(u, s, o, f, i)
#define MALLOC_DEFINE(t, s, l) NMX_MALLOC_DEFINE(t, s, l)
#define KASSERT(exp, msg) NMX_KASSERT(exp, msg)
#include <sys/kernel.h>
#include <sys/malloc.h>
#include <sys/ktr.h>
#include <sys/sched.h>
#include <sys/bus.h>
#include <sys/types.h>
#include <sys/kassert.h>
#include <machine/atomic.h>
#include <machine/cpufunc.h>
#include <sys/callout.h>
#include <sys/queue.h>
#include <sys/stdint.h>
#include <sys/libkern.h>
/* DELAY: busy-wait in microseconds, as FreeBSD's */
#define DELAY(usec) IODelay(usec)
void IODelay(unsigned microseconds);
int nmx_vprintf(const char *fmt, va_list ap);
#define vprintf(fmt, ap) nmx_vprintf((fmt), (ap))
int vsnprintf(char *str, size_t size, const char *format, va_list ap);
int vsprintf(char *buf, const char *cfmt, va_list ap);
int sscanf(const char *ibuf, const char *fmt, ...);
int vsscanf(const char *inp, char const *fmt0, va_list ap);
long strtol(const char *nptr, char **endptr, int base);
unsigned long strtoul(const char *nptr, char **endptr, int base);
long long strtoll(const char *nptr, char **endptr, int base);
unsigned long long strtoull(const char *nptr, char **endptr, int base);
#define strtoq strtoll
#define strtouq strtoull
/* FreeBSD's copyin/copyout take user pointers; xnu's take user_addr_t */
#define copyin(u, k, l) (copyin)((user_addr_t)(uintptr_t)(u), (k), (l))
#define copyout(k, u, l) (copyout)((k), (user_addr_t)(uintptr_t)(u), (l))
#define copyinstr(u, k, l, d) (copyinstr)((user_addr_t)(uintptr_t)(u), (k), (l), (d))
#define VM_MAXUSER_ADDRESS 0x00007ffffffff000ULL
int nmx_copyin_nofault(const void *uaddr, void *kaddr, size_t len);
int nmx_copyout_nofault(const void *kaddr, void *uaddr, size_t len);
#define copyin_nofault(u, k, l) nmx_copyin_nofault((u), (k), (l))
#define copyout_nofault(k, u, l) nmx_copyout_nofault((k), (u), (l))
int32_t fuword32(volatile const void *base);
int suword32(volatile void *base, int32_t word);
int64_t fuword64(volatile const void *base);
int suword64(volatile void *base, int64_t word);
/* the fueword forms report a fault separately from the value (fuword's -1 is also a valid word), so they copy in */
#define fueword32(b, v) (copyin((const void *)(b), (v), sizeof(int32_t)) ? -1 : 0)
#define fueword64(b, v) (copyin((const void *)(b), (v), sizeof(int64_t)) ? -1 : 0)
#define subyte(u, c) (subyte)((user_addr_t)(uintptr_t)(u), (c))
#define fubyte(u) (fubyte)((user_addr_t)(uintptr_t)(u))
#define bootverbose 0
#define SCHEDULER_STOPPED() 0
#define KERNEL_PANICKED() 0
#define panicstr ((const char *)0)
#define OSRELEASELEN 32
extern char osrelease[];
#define dumping 0
#define SV_CURPROC_FLAG(x) 0
#define SV_ILP32 0
#define SV_LP64 1
void kern_reboot(int howto) __dead2;
/* an orderly shutdown request (thermal emergencies): logged; macOS power management decides */
void shutdown_nice(int howto);
/* devd notifications: macOS has no devd; the event is logged */
void devctl_notify(const char *system, const char *subsystem, const char *type, const char *data);
static inline int abs(int a) { return a < 0 ? -a : a; }
char *strstr(const char *s, const char *find);
#define GID_VIDEO 44
/* FreeBSD places these in an ELF section; Mach-O needs segment,section names and gains nothing from it */
#define __read_mostly
#define __read_frequently
#define __exclusive_cache_line
#endif
