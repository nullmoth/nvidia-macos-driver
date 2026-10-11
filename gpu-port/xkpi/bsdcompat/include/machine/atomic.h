/*
 * FreeBSD <machine/atomic.h> API on xnu, built on the compiler's __atomic builtins.
 *
 * Semantics follow atomic(9): plain operations are relaxed, _acq orders later accesses after the operation, _rel orders
 * earlier accesses before it. On x86_64 every locked read-modify-write is a full barrier anyway, so the orderings cost
 * nothing extra; they are spelled out so the code stays correct if it is ever built for another architecture.
 * cmpset returns non-zero on success; fcmpset also writes the observed value back through 'old' on failure.
 */
#ifndef NMXKPI_MACHINE_ATOMIC_H
#define NMXKPI_MACHINE_ATOMIC_H

#include <stdint.h>

#define NMX_ATOMIC_OPS(name, type)                                                                                   \
static inline void atomic_add_##name(volatile type *p, type v) { __atomic_fetch_add(p, v, __ATOMIC_RELAXED); }        \
static inline void atomic_add_acq_##name(volatile type *p, type v) { __atomic_fetch_add(p, v, __ATOMIC_ACQUIRE); }    \
static inline void atomic_add_rel_##name(volatile type *p, type v) { __atomic_fetch_add(p, v, __ATOMIC_RELEASE); }    \
static inline void atomic_subtract_##name(volatile type *p, type v) { __atomic_fetch_sub(p, v, __ATOMIC_RELAXED); }   \
static inline void atomic_subtract_acq_##name(volatile type *p, type v) { __atomic_fetch_sub(p, v, __ATOMIC_ACQUIRE); } \
static inline void atomic_subtract_rel_##name(volatile type *p, type v) { __atomic_fetch_sub(p, v, __ATOMIC_RELEASE); } \
static inline void atomic_set_##name(volatile type *p, type v) { __atomic_fetch_or(p, v, __ATOMIC_RELAXED); }         \
static inline void atomic_set_acq_##name(volatile type *p, type v) { __atomic_fetch_or(p, v, __ATOMIC_ACQUIRE); }     \
static inline void atomic_set_rel_##name(volatile type *p, type v) { __atomic_fetch_or(p, v, __ATOMIC_RELEASE); }     \
static inline void atomic_clear_##name(volatile type *p, type v) { __atomic_fetch_and(p, (type)~v, __ATOMIC_RELAXED); } \
static inline void atomic_clear_acq_##name(volatile type *p, type v) { __atomic_fetch_and(p, (type)~v, __ATOMIC_ACQUIRE); } \
static inline void atomic_clear_rel_##name(volatile type *p, type v) { __atomic_fetch_and(p, (type)~v, __ATOMIC_RELEASE); } \
static inline int atomic_cmpset_##name(volatile type *p, type expect, type v) {                                        \
    return __atomic_compare_exchange_n(p, &expect, v, 0, __ATOMIC_SEQ_CST, __ATOMIC_RELAXED); }                         \
static inline int atomic_cmpset_acq_##name(volatile type *p, type expect, type v) {                                    \
    return __atomic_compare_exchange_n(p, &expect, v, 0, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED); }                         \
static inline int atomic_cmpset_rel_##name(volatile type *p, type expect, type v) {                                    \
    return __atomic_compare_exchange_n(p, &expect, v, 0, __ATOMIC_RELEASE, __ATOMIC_RELAXED); }                         \
static inline int atomic_fcmpset_##name(volatile type *p, type *expect, type v) {                                      \
    return __atomic_compare_exchange_n(p, expect, v, 0, __ATOMIC_SEQ_CST, __ATOMIC_RELAXED); }                          \
static inline int atomic_fcmpset_acq_##name(volatile type *p, type *expect, type v) {                                  \
    return __atomic_compare_exchange_n(p, expect, v, 0, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED); }                          \
static inline int atomic_fcmpset_rel_##name(volatile type *p, type *expect, type v) {                                  \
    return __atomic_compare_exchange_n(p, expect, v, 0, __ATOMIC_RELEASE, __ATOMIC_RELAXED); }                          \
static inline type atomic_fetchadd_##name(volatile type *p, type v) { return __atomic_fetch_add(p, v, __ATOMIC_SEQ_CST); } \
static inline type atomic_swap_##name(volatile type *p, type v) { return __atomic_exchange_n(p, v, __ATOMIC_SEQ_CST); } \
static inline type atomic_readandclear_##name(volatile type *p) { return __atomic_exchange_n(p, (type)0, __ATOMIC_SEQ_CST); } \
static inline type atomic_load_##name(const volatile type *p) { return __atomic_load_n(p, __ATOMIC_RELAXED); }         \
static inline type atomic_load_acq_##name(const volatile type *p) { return __atomic_load_n(p, __ATOMIC_ACQUIRE); }     \
static inline void atomic_store_##name(volatile type *p, type v) { __atomic_store_n(p, v, __ATOMIC_RELAXED); }         \
static inline void atomic_store_rel_##name(volatile type *p, type v) { __atomic_store_n(p, v, __ATOMIC_RELEASE); }     \
static inline int atomic_testandset_##name(volatile type *p, unsigned bit) {                                           \
    type m = (type)((type)1 << (bit % (sizeof(type) * 8)));                                                             \
    return (__atomic_fetch_or(p, m, __ATOMIC_SEQ_CST) & m) != 0; }                                                       \
static inline int atomic_testandclear_##name(volatile type *p, unsigned bit) {                                         \
    type m = (type)((type)1 << (bit % (sizeof(type) * 8)));                                                             \
    return (__atomic_fetch_and(p, (type)~m, __ATOMIC_SEQ_CST) & m) != 0; }

NMX_ATOMIC_OPS(char, unsigned char)
NMX_ATOMIC_OPS(short, unsigned short)
NMX_ATOMIC_OPS(int, unsigned int)
NMX_ATOMIC_OPS(long, unsigned long)
NMX_ATOMIC_OPS(8, uint8_t)
NMX_ATOMIC_OPS(16, uint16_t)
NMX_ATOMIC_OPS(32, uint32_t)
NMX_ATOMIC_OPS(64, uint64_t)
NMX_ATOMIC_OPS(ptr, uintptr_t)
#undef NMX_ATOMIC_OPS

/* FreeBSD's atomic_*_ptr take the address of any pointer-sized object; callers cast as they do on FreeBSD. */

static inline void atomic_thread_fence_acq(void) { __atomic_thread_fence(__ATOMIC_ACQUIRE); }
static inline void atomic_thread_fence_rel(void) { __atomic_thread_fence(__ATOMIC_RELEASE); }
static inline void atomic_thread_fence_acq_rel(void) { __atomic_thread_fence(__ATOMIC_ACQ_REL); }
static inline void atomic_thread_fence_seq_cst(void) { __atomic_thread_fence(__ATOMIC_SEQ_CST); }
static inline void atomic_interrupt_fence(void) { __atomic_signal_fence(__ATOMIC_SEQ_CST); }

/* x86 memory barriers as FreeBSD spells them */
#define mb() __asm__ __volatile__("mfence" ::: "memory")
#define rmb() __asm__ __volatile__("lfence" ::: "memory")
#define wmb() __asm__ __volatile__("sfence" ::: "memory")

#endif
