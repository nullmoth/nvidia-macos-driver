/* FreeBSD <sys/cdefs.h> on xnu: xnu's, plus the FreeBSD attribute and helper macros LinuxKPI uses. */
#ifndef NMXKPI_SYS_CDEFS_H
#define NMXKPI_SYS_CDEFS_H
#include_next <sys/cdefs.h>
/* FreeBSD's two-level __CONCAT expands its arguments first, so nested uses paste names, not parentheses;
 * xnu's pastes directly. LinuxKPI's cleanup guards nest it. */
#undef __CONCAT
#define __CONCAT1(x, y) x ## y
#define __CONCAT(x, y) __CONCAT1(x, y)
#ifndef __aligned
#define __aligned(x) __attribute__((__aligned__(x)))
#endif
#ifndef __packed
#define __packed __attribute__((__packed__))
#endif
#ifndef __section
#define __section(x) __attribute__((__section__(x)))
#endif
#ifndef __always_inline
#define __always_inline __inline __attribute__((__always_inline__))
#endif
#ifndef __noinline
#define __noinline __attribute__((__noinline__))
#endif
#ifndef __pure
#define __pure __attribute__((__pure__))
#endif
#ifndef __pure2
#define __pure2 __attribute__((__const__))
#endif
#ifndef __dead2
#define __dead2 __attribute__((__noreturn__))
#endif
#ifndef __printflike
#define __printflike(fmtarg, firstvararg) __attribute__((__format__(__printf__, fmtarg, firstvararg)))
#endif
#ifndef __predict_true
#define __predict_true(exp) __builtin_expect((exp), 1)
#define __predict_false(exp) __builtin_expect((exp), 0)
#endif
#ifndef __containerof
#define __containerof(x, s, m) ((s *)((char *)(x) - __builtin_offsetof(s, m)))
#endif
#ifndef __DECONST
#define __DECONST(type, var) ((type)(__uintptr_t)(const void *)(var))
#endif
#ifndef __DEVOLATILE
#define __DEVOLATILE(type, var) ((type)(__uintptr_t)(volatile void *)(var))
#endif
#ifndef __DEQUALIFY
#define __DEQUALIFY(type, var) ((type)(__uintptr_t)(const volatile void *)(var))
#endif
#ifndef __offsetof
#define __offsetof(type, field) __builtin_offsetof(type, field)
#endif
#ifndef __compiler_membar
#define __compiler_membar() __asm__ __volatile__(" " : : : "memory")
#endif
#ifndef __maybe_unused
#define __maybe_unused __attribute__((__unused__))
#endif
#ifndef __diagused
#define __diagused __attribute__((__unused__))
#endif
#ifndef __result_use_check
#define __result_use_check __attribute__((__warn_unused_result__))
#endif
#ifndef __weak_symbol
#define __weak_symbol __attribute__((__weak__))
#endif
#ifndef __nonstring
#define __nonstring
#endif
#ifndef __exported
#define __exported __attribute__((__visibility__("default")))
#endif
#ifndef __hidden
#define __hidden __attribute__((__visibility__("hidden")))
#endif
#ifndef __XSTRING
#define __XSTRING(x) __STRING(x)
#endif
#ifndef __bitcount32
#define __bitcount32(x) __builtin_popcount((unsigned int)(x))
#define __bitcount64(x) __builtin_popcountll((unsigned long long)(x))
#define __bitcountl(x) __builtin_popcountl((unsigned long)(x))
#define __bitcount(x) __builtin_popcount((unsigned int)(x))
#endif
#define bitcount16(x) __builtin_popcount((unsigned)(uint16_t)(x))
#define bitcount32(x) __builtin_popcount((unsigned)(x))
#define bitcount64(x) __builtin_popcountll((unsigned long long)(x))
#define bitcountl(x) __builtin_popcountl((unsigned long)(x))
#define bitcount(x) __builtin_popcount((unsigned)(x))
#ifndef __builtin_is_aligned
#define __is_aligned(x, align) (((__uintptr_t)(x) & ((align) - 1)) == 0)
#endif
#ifndef __unreachable
#define __unreachable() __builtin_unreachable()
#endif
#ifndef __assert_unreachable
#define __assert_unreachable() __builtin_unreachable()
#endif
#ifndef __no_lock_analysis
#define __no_lock_analysis
#endif
#ifndef __guarded_by
#define __guarded_by(x)
#define __pt_guarded_by(x)
#define __lockable
#define __locks_exclusive(...)
#define __locks_shared(...)
#define __trylocks_exclusive(...)
#define __trylocks_shared(...)
#define __unlocks(...)
#define __asserts_exclusive(...)
#define __asserts_shared(...)
#define __requires_exclusive(...)
#define __requires_shared(...)
#define __requires_unlocked(...)
#endif
#endif
