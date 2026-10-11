/* FreeBSD UMA alignment: kmalloc alignment in this port (see src/nmx_malloc.c). */
#pragma once
#define UMA_ALIGN_CACHE (64 - 1)
#define UMA_ALIGN_PTR (sizeof(void *) - 1)
#define uma_get_cache_align_mask() UMA_ALIGN_CACHE
