/*
 * FreeBSD malloc(9) on xnu.
 *
 * malloc/free/realloc here are real functions with FreeBSD's signatures (a type and flags), implemented in
 * src/nmx_malloc.c over IOMalloc: each block carries a header with its size, so free() needs no size, and the
 * returned address is aligned to 16 bytes, or to the block's own power-of-two size up to a page, which is the
 * alignment Linux's kmalloc guarantees and drivers rely on. xnu's sys/malloc.h (_MALLOC and integer M_* types) is
 * deliberately not included: FreeBSD's M_* names are malloc_type objects.
 */
#ifndef NMXKPI_SYS_MALLOC_H
#define NMXKPI_SYS_MALLOC_H
#ifndef _SYS_MALLOC_H_
#define _SYS_MALLOC_H_   /* xnu's guard: keep its header out */
#endif
#include <sys/types.h>
#define M_NOWAIT 0x0001
#define M_WAITOK 0x0002
#define M_NORECLAIM 0x0080
#define M_ZERO 0x0100
#define M_NOVM 0x0200
#define M_USE_RESERVE 0x0400
#define M_NODUMP 0x0800
#define M_FIRSTFIT 0x1000
#define M_BESTFIT 0x2000
#define M_EXEC 0x4000
#define M_NEXTFIT 0x8000
#define M_VERSION 2020110501
struct malloc_type {
    struct malloc_type *ks_next;
    u_long ks_version;
    const char *ks_shortdesc;
    volatile uint64_t ks_inuse;     /* bytes currently allocated with this type */
};
#undef MALLOC_DEFINE
#define NMX_MALLOC_DEFINE(type, shortdesc, longdesc) \
    struct malloc_type type[1] = { { NULL, M_VERSION, (shortdesc), 0 } }
#define MALLOC_DEFINE(type, shortdesc, longdesc) NMX_MALLOC_DEFINE(type, shortdesc, longdesc)
#define MALLOC_DECLARE(type) extern struct malloc_type type[1]
MALLOC_DECLARE(M_TEMP);
MALLOC_DECLARE(M_DEVBUF);
#undef malloc
#undef free
#undef realloc
void *malloc(size_t size, struct malloc_type *type, int flags) __attribute__((__malloc__, __alloc_size__(1)));
void free(void *addr, struct malloc_type *type);
void zfree(void *addr, struct malloc_type *type);
void *realloc(void *addr, size_t size, struct malloc_type *type, int flags) __attribute__((__alloc_size__(2)));
void *reallocf(void *addr, size_t size, struct malloc_type *type, int flags) __attribute__((__alloc_size__(2)));
void *malloc_aligned(size_t size, size_t align, struct malloc_type *type, int flags);
size_t malloc_usable_size(const void *addr);
/* the size malloc() would really hand out for a request (its bucket); LinuxKPI's ksize() */
size_t malloc_size(size_t size);
void *contigmalloc(unsigned long size, struct malloc_type *type, int flags, vm_paddr_t low, vm_paddr_t high, unsigned long alignment, vm_paddr_t boundary) __attribute__((__malloc__));
void contigfree(void *addr, unsigned long size, struct malloc_type *type);
struct domainset;
#define malloc_domainset(size, type, ds, flags) malloc((size), (type), (flags))
#define malloc_domainset_aligned(size, align, type, ds, flags) malloc_aligned((size), (align), (type), (flags))
#define contigmalloc_domainset(size, type, ds, flags, low, high, align, bnd) contigmalloc((size), (type), (flags), (low), (high), (align), (bnd))
#define malloc_exec(size, type, flags) malloc((size), (type), (flags))
#define MUL_NO_OVERFLOW (1UL << (sizeof(size_t) * 8 / 2))
static inline bool WOULD_OVERFLOW(size_t nmemb, size_t size)
{
    return ((nmemb >= MUL_NO_OVERFLOW || size >= MUL_NO_OVERFLOW) && nmemb > 0 && __SIZE_MAX__ / nmemb < size);
}
static inline void *mallocarray(size_t nmemb, size_t size, struct malloc_type *type, int flags)
{
    if (WOULD_OVERFLOW(nmemb, size)) return (NULL);
    return (malloc(size * nmemb, type, flags));
}
#endif
