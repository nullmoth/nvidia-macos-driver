/* FreeBSD sema(9) on xnu: a counting semaphore built from a sleep mutex and a condition variable. */
#pragma once
#include <sys/_mutex.h>
#include <sys/condvar.h>
struct sema {
    struct mtx sema_mtx;
    struct cv sema_cv;
    int sema_waiters;
    int sema_value;
};
void sema_init(struct sema *sema, int value, const char *description);
void sema_destroy(struct sema *sema);
void _sema_post(struct sema *sema, const char *file, int line);
void _sema_wait(struct sema *sema, const char *file, int line);
int _sema_timedwait(struct sema *sema, int timo, const char *file, int line);
int _sema_trywait(struct sema *sema, const char *file, int line);
int sema_value(struct sema *sema);
#define sema_post(s) _sema_post((s), __FILE__, __LINE__)
#define sema_wait(s) _sema_wait((s), __FILE__, __LINE__)
#define sema_timedwait(s, t) _sema_timedwait((s), (t), __FILE__, __LINE__)
#define sema_trywait(s) _sema_trywait((s), __FILE__, __LINE__)
