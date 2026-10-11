/* FreeBSD <sys/param.h> on xnu: xnu's, with the FreeBSD forms of the arithmetic helpers (xnu's MIN/MAX and friends
 * are replaced by FreeBSD's identical-meaning definitions so later headers see one consistent set). */
#ifndef NMXKPI_SYS_PARAM_H
#define NMXKPI_SYS_PARAM_H
#include_next <sys/param.h>
#include <sys/types.h>
#undef howmany
#undef roundup
#undef rounddown
#undef powerof2
#undef MIN
#undef MAX
#define howmany(x, y) (((x) + ((y) - 1)) / (y))
#define nitems(x) (sizeof((x)) / sizeof((x)[0]))
#define rounddown(x, y) (((x) / (y)) * (y))
#define rounddown2(x, y) ((x) & ~((y) - 1))
#define roundup(x, y) ((((x) + ((y) - 1)) / (y)) * (y))
#define roundup2(x, y) (((x) + ((y) - 1)) & ~((y) - 1))
#define powerof2(x) ((((x) - 1) & (x)) == 0)
#define MIN(a, b) (((a) < (b)) ? (a) : (b))
#define MAX(a, b) (((a) > (b)) ? (a) : (b))
#ifndef PAGE_MASK
#define PAGE_MASK (PAGE_SIZE - 1)
#endif
#define trunc_page(x) ((uintptr_t)(x) & ~(uintptr_t)PAGE_MASK)
#define round_page(x) (((uintptr_t)(x) + PAGE_MASK) & ~(uintptr_t)PAGE_MASK)
#define atop(x) ((uintptr_t)(x) >> PAGE_SHIFT)
#define ptoa(x) ((uintptr_t)(x) << PAGE_SHIFT)
#define MAXCPU 256
#define MAXCOMLEN 19
#define CACHE_LINE_SIZE 64
#define CACHE_LINE_SHIFT 6
#ifndef hz
extern int hz;
#endif
#endif
