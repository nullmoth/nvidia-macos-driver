/*
 * FreeBSD sleepqueue(9) on xnu. A wait channel is any address. sleepq_lock() takes a spin lock from a hashed table;
 * the waiting thread calls assert_wait on the channel while still holding it, so a wakeup can never fall between
 * "decided to sleep" and "asleep". Waiter counts per channel make sleepq_sleepcnt() exact.
 */
#pragma once
#include <sys/types.h>
#include <sys/lock.h>
#define SLEEPQ_TYPE 0x0ff
#define SLEEPQ_SLEEP 0x00
#define SLEEPQ_CONDVAR 0x01
#define SLEEPQ_PAUSE 0x02
#define SLEEPQ_SX 0x03
#define SLEEPQ_LK 0x04
#define SLEEPQ_INTERRUPTIBLE 0x100
#define SLEEPQ_UNFAIR 0x200
#define SLEEPQ_DROP 0x400
struct sleepqueue;
struct thread;
void sleepq_lock(const void *wchan);
void sleepq_release(const void *wchan);
void sleepq_add(const void *wchan, struct lock_object *lock, const char *wmesg, int flags, int queue);
void sleepq_set_timeout_sbt(const void *wchan, sbintime_t sbt, sbintime_t pr, int flags);
#define sleepq_set_timeout(wchan, timo) sleepq_set_timeout_sbt((wchan), tick_sbt * (timo), 0, C_HARDCLOCK)
u_int sleepq_sleepcnt(const void *wchan, int queue);
int sleepq_signal(const void *wchan, int flags, int pri, int queue);
int sleepq_broadcast(const void *wchan, int flags, int pri, int queue);
void sleepq_wait(const void *wchan, int pri);
int sleepq_wait_sig(const void *wchan, int pri);
int sleepq_timedwait(const void *wchan, int pri);
int sleepq_timedwait_sig(const void *wchan, int pri);
void sleepq_remove(struct thread *td, const void *wchan);
int sleepq_type(const void *wchan);
