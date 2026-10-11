/* FreeBSD select/kqueue bookkeeping as LinuxKPI embeds it in each open file (poll wakeups go through xnu's selwakeup). */
#ifndef NMXKPI_SYS_SELINFO_H
#define NMXKPI_SYS_SELINFO_H
#include <sys/types.h>
#include <sys/queue.h>
struct knote;
SLIST_HEAD(klist, knote);
struct knlist {
    struct klist kl_list;
    void (*kl_lock)(void *);
    void (*kl_unlock)(void *);
    void (*kl_assert_lock)(void *, int);
    void *kl_lockarg;
    int kl_autodestroy;
};
struct selinfo {
    void *si_xnu;                   /* xnu struct selinfo storage, allocated by the poll glue */
    struct knlist si_note;
    struct mtx *si_mtx;
};
#define SEL_WAITING(si) ((si)->si_xnu != NULL)
/* xnu declares selrecord/selwakeup with its own selinfo; the FreeBSD forms are the port's */
void nmx_selrecord(struct thread *selector, struct selinfo *sip);
void nmx_selwakeup(struct selinfo *sip);
void nmx_seldrain(struct selinfo *sip);
#define selrecord(td, sip) nmx_selrecord((td), (sip))
#define selwakeup(sip) nmx_selwakeup(sip)
#define selwakeuppri(sip, pri) nmx_selwakeup(sip)
#define seldrain(sip) nmx_seldrain(sip)
#define KNOTE_LOCKED(list, hint) do { } while (0)
#define KNOTE_UNLOCKED(list, hint) do { } while (0)
void knlist_init(struct knlist *knl, void *lock, void (*kl_lock)(void *), void (*kl_unlock)(void *), void (*kl_assert_lock)(void *, int));
void knlist_destroy(struct knlist *knl);
void knlist_clear(struct knlist *knl, int islocked);
#define knlist_delete(knl, td, islocked) knlist_clear((knl), (islocked))
#endif
