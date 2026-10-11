/*
 * FreeBSD <sys/libkern.h> on xnu: xnu's libkern (which already has imax/imin/lmax/lmin/ulmax/ulmin, ffs/ffsll/fls/flsll,
 * crc32, bcmp, strcasecmp, strlcpy, strnlen), plus the FreeBSD helpers it lacks. Bit scans use compiler builtins and
 * return 1-based positions, 0 when no bit is set.
 */
#ifndef NMXKPI_SYS_LIBKERN_H
#define NMXKPI_SYS_LIBKERN_H
#include <libkern/libkern.h>
#include <sys/types.h>
#include <string.h>
static inline quad_t qmax(quad_t a, quad_t b) { return (a > b ? a : b); }
static inline quad_t qmin(quad_t a, quad_t b) { return (a < b ? a : b); }
static inline u_quad_t uqmax(u_quad_t a, u_quad_t b) { return (a > b ? a : b); }
static inline u_quad_t uqmin(u_quad_t a, u_quad_t b) { return (a < b ? a : b); }
static inline off_t omax(off_t a, off_t b) { return (a > b ? a : b); }
static inline off_t omin(off_t a, off_t b) { return (a < b ? a : b); }
#define ffsl(x) __builtin_ffsl((long)(x))
static inline int flsl(long mask) { return (mask == 0 ? 0 : (int)(sizeof(mask) * 8) - __builtin_clzl((unsigned long)mask)); }
uint32_t arc4random(void);
uint32_t arc4random_uniform(uint32_t upper_bound);
void arc4random_buf(void *ptr, size_t len);
void qsort_r(void *base, size_t nmemb, size_t size, int (*compar)(const void *, const void *, void *), void *thunk);
void *bsearch(const void *key, const void *base, size_t n, size_t size, int (*cmp)(const void *, const void *));
u_long nmx_random(void);
#define random() nmx_random()
char *strsep(char **stringp, const char *delim);
char *strrchr(const char *s, int c);
struct malloc_type;
char *strdup_flags(const char *string, struct malloc_type *type, int flags);
#define strdup(s, type) strdup_flags((s), (type), 0x0002 /* M_WAITOK */)
char *strpbrk(const char *s, const char *charset);
size_t strspn(const char *s, const char *charset);
size_t strcspn(const char *s, const char *charset);
char *strcasestr(const char *s, const char *find);
void *memmem(const void *l, size_t l_len, const void *s, size_t s_len);
void *memrchr(const void *s, int c, size_t n);
char *strchrnul(const char *s, int c);
#define index(s, c) strchr((s), (c))
#define rindex(s, c) strrchr((s), (c))
uint32_t crc32_raw(const void *buf, size_t size, uint32_t crc);

/* integer log2 helpers, as FreeBSD sys/libkern.h defines them (BSD-3-Clause); LinuxKPI's linux/log2.h relies on them */
static __inline int ilog2_int(int n) { return (int)(8 * sizeof(n) - 1 - __builtin_clz((unsigned)n)); }
static __inline int ilog2_long(long n) { return (int)(8 * sizeof(n) - 1 - __builtin_clzl((unsigned long)n)); }
static __inline int ilog2_long_long(long long n) { return (int)(8 * sizeof(n) - 1 - __builtin_clzll((unsigned long long)n)); }
#define ilog2_var(n) _Generic((n), default: ilog2_int, long: ilog2_long, unsigned long: ilog2_long, \
    long long: ilog2_long_long, unsigned long long: ilog2_long_long)(n)
#define ilog2_const(n) \
    ((sizeof(n) > sizeof(long) ? 8 * sizeof(long long) - __builtin_clzll(n) : \
      sizeof(n) > sizeof(int) ? 8 * sizeof(long) - __builtin_clzl(n) : 8 * sizeof(int) - __builtin_clz(n)) - 1)
#define ilog2(n) (__builtin_constant_p(n) ? ilog2_const(n) : ilog2_var(n))
#define rounddown_pow_of_two(n) ((__typeof(n))1 << ilog2(n))
#define order_base_2(n) ilog2(2 * (n) - 1)
#define roundup_pow_of_two(n) ((__typeof(n))1 << order_base_2(n))
#endif
