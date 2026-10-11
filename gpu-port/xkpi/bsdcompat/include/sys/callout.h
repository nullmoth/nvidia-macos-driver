/* FreeBSD callout(9) on xnu. Timeouts are in ticks (hz = 1000) or sbintime_t, as on FreeBSD. */
#pragma once
#include <sys/_callout.h>
#include <sys/time.h>
#define CALLOUT_LOCAL_ALLOC 0x0001
#define CALLOUT_ACTIVE 0x0002
#define CALLOUT_PENDING 0x0004
#define CALLOUT_MPSAFE 0x0008
#define CALLOUT_RETURNUNLOCKED 0x0010
#define CALLOUT_SHAREDLOCK 0x0020
#define CALLOUT_DFRMIGRATION 0x0040
#define CALLOUT_PROCESSED 0x0080
#define CALLOUT_DIRECT 0x0100
#define CALLOUT_TRYLOCK 0x0400
#define C_DIRECT_EXEC 0x0001
#define C_PRELBITS 7
#define C_PRELRANGE ((1 << C_PRELBITS) - 1)
#define C_PREL(x) (((x) + 1) << 1)
#define C_PRECALC 0x0100
#define C_CATCH 0x0200
#define C_ABSOLUTE 0x0400
#define C_HARDCLOCK 0x0800
#define C_CANCELED 0x1000
#define callout_active(c) ((c)->c_flags & CALLOUT_ACTIVE)
#define callout_deactivate(c) __atomic_fetch_and(&(c)->c_flags, ~CALLOUT_ACTIVE, __ATOMIC_SEQ_CST)
#define callout_pending(c) ((c)->c_flags & CALLOUT_PENDING)
void callout_init(struct callout *c, int mpsafe);
void _callout_init_lock(struct callout *c, struct lock_object *lock, int flags);
#define callout_init_mtx(c, mtx, flags) _callout_init_lock((c), ((mtx) != NULL) ? &(mtx)->lock_object : NULL, (flags))
#define callout_init_rw(c, rw, flags) _callout_init_lock((c), ((rw) != NULL) ? &(rw)->lock_object : NULL, (flags))
int callout_reset_sbt_on(struct callout *c, sbintime_t sbt, sbintime_t prec, void (*func)(void *), void *arg, int cpu, int flags);
#define callout_reset_sbt(c, sbt, pr, fn, arg, flags) callout_reset_sbt_on((c), (sbt), (pr), (fn), (arg), -1, (flags))
#define callout_reset_on(c, to_ticks, fn, arg, cpu) callout_reset_sbt_on((c), tick_sbt * (to_ticks), 0, (fn), (arg), (cpu), C_HARDCLOCK)
#define callout_reset(c, on_tick, fn, arg) callout_reset_on((c), (on_tick), (fn), (arg), -1)
#define callout_reset_curcpu(c, on_tick, fn, arg) callout_reset_on((c), (on_tick), (fn), (arg), -1)
#define callout_schedule(c, on_tick) callout_reset((c), (on_tick), (c)->c_func, (c)->c_arg)
#define callout_schedule_sbt(c, sbt, pr, flags) callout_reset_sbt((c), (sbt), (pr), (c)->c_func, (c)->c_arg, (flags))
int _callout_stop_safe(struct callout *c, int flags);
#define CS_DRAIN 0x0001
#define CS_EXECUTING 0x0002
#define callout_stop(c) _callout_stop_safe((c), 0)
#define callout_drain(c) _callout_stop_safe((c), CS_DRAIN)
int callout_async_drain(struct callout *c, void (*drain)(void *));
#define callout_when(sbt, pr, flags, sbt_res, prec_res) do { *(sbt_res) = (sbt); *(prec_res) = (pr); } while (0)
