/* nvaccel_lock_owner.h - which thread holds the accelerator lock, so a long wait can let go of it safely.
   The family calls acceleratorDidLock / acceleratorWillUnlock around its own locking of the accelerator. A page-off that
   must wait for the GPU (up to 100 ms) used to wait with that lock held, and every RM call that needs the lock queued
   behind it. It may let go only when the CURRENT thread is the recorded owner and holds exactly one level: a thread that
   is not the owner holds nothing to give up, and a thread holding it twice would still hold it after one unlock.
   Pure (the thread identity is a pointer the caller supplies), so tools/test_accel_lock_owner.c proves the rules and the
   order of events on the host. */
#ifndef NVACCEL_LOCK_OWNER_H
#define NVACCEL_LOCK_OWNER_H
#include <stdint.h>
typedef struct { void *volatile owner; volatile int depth; } nvaccel_lock_owner;

static inline void nvaccel_owner_did_lock(nvaccel_lock_owner *o, void *self)
{
    /* the caller has just taken the lock, so no other thread is between its own did_lock and will_unlock */
    if (__atomic_load_n(&o->owner, __ATOMIC_ACQUIRE) == self) __atomic_add_fetch(&o->depth, 1, __ATOMIC_RELAXED);
    else { __atomic_store_n(&o->depth, 1, __ATOMIC_RELAXED); __atomic_store_n(&o->owner, self, __ATOMIC_RELEASE); }
}
static inline void nvaccel_owner_will_unlock(nvaccel_lock_owner *o, void *self)
{
    /* only the owner's unlock changes anything: a stray call from another thread must not clear the owner */
    if (__atomic_load_n(&o->owner, __ATOMIC_ACQUIRE) != self) return;
    if (__atomic_sub_fetch(&o->depth, 1, __ATOMIC_RELAXED) <= 0) { __atomic_store_n(&o->depth, 0, __ATOMIC_RELAXED); __atomic_store_n(&o->owner, (void *)0, __ATOMIC_RELEASE); }
}
/* 1 when `self` may let go of the lock for a long wait: it is the owner and holds exactly one level. */
static inline int nvaccel_owner_may_release(const nvaccel_lock_owner *o, void *self)
{
    return __atomic_load_n(&o->owner, __ATOMIC_ACQUIRE) == self && __atomic_load_n(&o->depth, __ATOMIC_RELAXED) == 1;
}
#endif
