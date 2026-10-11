/* FreeBSD condvar(9) on xnu: waits release the caller's lock (any class) and sleep on the cv's address. */
#pragma once
#include <sys/types.h>
#include <sys/lock.h>
struct cv {
    const char *cv_description;
    volatile int cv_waiters;
};
void cv_init(struct cv *cvp, const char *desc);
void cv_destroy(struct cv *cvp);
void _cv_wait(struct cv *cvp, struct lock_object *lock);
void _cv_wait_unlock(struct cv *cvp, struct lock_object *lock);
int _cv_wait_sig(struct cv *cvp, struct lock_object *lock);
int _cv_timedwait_sbt(struct cv *cvp, struct lock_object *lock, sbintime_t sbt, sbintime_t pr, int flags);
int _cv_timedwait_sig_sbt(struct cv *cvp, struct lock_object *lock, sbintime_t sbt, sbintime_t pr, int flags);
void cv_signal(struct cv *cvp);
void cv_broadcastpri(struct cv *cvp, int pri);
#define cv_wait(cvp, lock) _cv_wait((cvp), &(lock)->lock_object)
#define cv_wait_unlock(cvp, lock) _cv_wait_unlock((cvp), &(lock)->lock_object)
#define cv_wait_sig(cvp, lock) _cv_wait_sig((cvp), &(lock)->lock_object)
#define cv_timedwait(cvp, lock, timo) _cv_timedwait_sbt((cvp), &(lock)->lock_object, tick_sbt * (timo), 0, C_HARDCLOCK)
#define cv_timedwait_sbt(cvp, lock, sbt, pr, flags) _cv_timedwait_sbt((cvp), &(lock)->lock_object, (sbt), (pr), (flags))
#define cv_timedwait_sig(cvp, lock, timo) _cv_timedwait_sig_sbt((cvp), &(lock)->lock_object, tick_sbt * (timo), 0, C_HARDCLOCK)
#define cv_timedwait_sig_sbt(cvp, lock, sbt, pr, flags) _cv_timedwait_sig_sbt((cvp), &(lock)->lock_object, (sbt), (pr), (flags))
#define cv_broadcast(cvp) cv_broadcastpri((cvp), 0)
#define cv_wmesg(cvp) ((cvp)->cv_description)
