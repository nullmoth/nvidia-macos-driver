/* FreeBSD counter(9) on xnu: a 64-bit statistics counter, updated atomically (FreeBSD keeps one per CPU) */
#ifndef NMXKPI_SYS_COUNTER_H
#define NMXKPI_SYS_COUNTER_H
#include <sys/types.h>
typedef uint64_t *counter_u64_t;
counter_u64_t counter_u64_alloc(int flags);
void counter_u64_free(counter_u64_t c);
#define counter_u64_add(c, v) __atomic_fetch_add((c), (uint64_t)(v), __ATOMIC_RELAXED)
#define counter_u64_fetch(c) __atomic_load_n((c), __ATOMIC_RELAXED)
#define counter_u64_zero(c) __atomic_store_n((c), 0, __ATOMIC_RELAXED)
#endif
