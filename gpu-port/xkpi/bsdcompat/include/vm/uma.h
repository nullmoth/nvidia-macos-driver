/*
 * FreeBSD UMA (zone allocator) on xnu: LinuxKPI builds kmem_cache on it. A zone here is a fixed-size allocator over
 * the port's kmalloc, with constructor/destructor calls, which is everything kmem_cache relies on.
 */
#ifndef NMXKPI_VM_UMA_H
#define NMXKPI_VM_UMA_H
#include <sys/types.h>
typedef struct uma_zone *uma_zone_t;
typedef int (*uma_ctor)(void *mem, int size, void *arg, int flags);
typedef void (*uma_dtor)(void *mem, int size, void *arg);
typedef int (*uma_init)(void *mem, int size, int flags);
typedef void (*uma_fini)(void *mem, int size);
typedef int (*uma_import)(void *arg, void **store, int count, int domain, int flags);
typedef void (*uma_release)(void *arg, void **store, int count);
uma_zone_t uma_zcreate(const char *name, size_t size, uma_ctor ctor, uma_dtor dtor, uma_init uminit, uma_fini fini, int align, uint32_t flags);
uma_zone_t uma_zcache_create(const char *name, int size, uma_ctor ctor, uma_dtor dtor, uma_init zinit, uma_fini zfini,
                             uma_import zimport, uma_release zrelease, void *arg, int flags);
void uma_zdestroy(uma_zone_t zone);
void *uma_zalloc_arg(uma_zone_t zone, void *arg, int flags);
void uma_zfree_arg(uma_zone_t zone, void *item, void *arg);
#define uma_zalloc(z, f) uma_zalloc_arg((z), NULL, (f))
#define uma_zfree(z, i) uma_zfree_arg((z), (i), NULL)
#define uma_zone_reclaim(z, req) do { } while (0)
#define UMA_ALIGN_PTR (sizeof(void *) - 1)
#define UMA_ALIGN_LONG (sizeof(long) - 1)
#define UMA_ALIGN_CACHE (64 - 1)
#define UMA_ZONE_ZINIT 0x0002
#define UMA_ZONE_NOFREE 0x0020
#define UMA_ZONE_MAXBUCKET 0x0800
#define UMA_RECLAIM_DRAIN 1
#endif
