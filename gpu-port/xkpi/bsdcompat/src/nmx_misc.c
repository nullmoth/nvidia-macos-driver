/*
 * Smaller FreeBSD kernel services on xnu: strings, randomness, CRC-32, event handlers, CPU rendezvous, kernel
 * threads, privilege checks, vnode reference counts, and the file-descriptor and select entry points that macOS
 * does not offer kexts.
 */
#include <sys/param.h>
#include <sys/systm.h>
#include <sys/malloc.h>
#include <sys/lock.h>
#include <sys/mutex.h>
#include <sys/proc.h>
#include <sys/kthread.h>
#include <sys/priv.h>
#include <sys/smp.h>
#include <sys/eventhandler.h>
#include <sys/interrupt.h>
#include <sys/power.h>
#include <sys/vnode.h>
#include <sys/file.h>
#include <sys/selinfo.h>
#include <sys/capsicum.h>
#include <vm/swap_pager.h>
#include <sys/kauth.h>

MALLOC_DEFINE(M_NMXMISC, "nmxmisc", "compatibility layer bookkeeping");

void nmx_thread_exit_cleanup(void);
extern void read_random(void *buffer, u_int numBytes);
extern void mp_rendezvous_no_intrs(void (*action)(void *), void *arg);

/* ---- strings (FreeBSD libkern) ------------------------------------------------------------------------------------ */

char *strsep(char **stringp, const char *delim)
{
    char *s = *stringp;
    if (s == NULL)
        return NULL;
    for (char *t = s; ; t++) {
        char c = *t;
        const char *d = delim;
        do {
            if (*d == c) {
                if (c == '\0')
                    *stringp = NULL;
                else {
                    *t = '\0';
                    *stringp = t + 1;
                }
                return s;
            }
        } while (*d++ != '\0');
    }
}

char *strpbrk(const char *s, const char *charset)
{
    for (; *s; s++)
        for (const char *c = charset; *c; c++)
            if (*s == *c)
                return (char *)(uintptr_t)s;
    return NULL;
}

size_t strspn(const char *s, const char *charset)
{
    size_t n = 0;
    for (; s[n]; n++) {
        const char *c = charset;
        while (*c && *c != s[n])
            c++;
        if (*c == '\0')
            break;
    }
    return n;
}

size_t strcspn(const char *s, const char *charset)
{
    size_t n = 0;
    for (; s[n]; n++)
        for (const char *c = charset; *c; c++)
            if (*c == s[n])
                return n;
    return n;
}

/* xnu offers kexts neither strrchr nor strstr */
char *strrchr(const char *s, int c)
{
    const char *last = NULL;
    do {
        if (*s == (char)c)
            last = s;
    } while (*s++);
    return (char *)(uintptr_t)last;
}

char *strstr(const char *s, const char *find)
{
    return strnstr(s, find, strlen(s) + 1);
}

char *strchrnul(const char *s, int c)
{
    while (*s && *s != (char)c)
        s++;
    return (char *)(uintptr_t)s;
}

char *strcasestr(const char *s, const char *find)
{
    size_t n = strlen(find);
    for (; *s; s++)
        if (strncasecmp(s, find, n) == 0)
            return (char *)(uintptr_t)s;
    return n == 0 ? (char *)(uintptr_t)s : NULL;
}

void *memmem(const void *l, size_t l_len, const void *s, size_t s_len)
{
    const char *cl = l, *cs = s;
    if (s_len == 0)
        return (void *)(uintptr_t)l;
    if (l_len < s_len)
        return NULL;
    for (size_t i = 0; i <= l_len - s_len; i++)
        if (cl[i] == cs[0] && memcmp(cl + i, cs, s_len) == 0)
            return (void *)(uintptr_t)(cl + i);
    return NULL;
}

void *memrchr(const void *s, int c, size_t n)
{
    const unsigned char *p = (const unsigned char *)s + n;
    while (n--)
        if (*--p == (unsigned char)c)
            return (void *)(uintptr_t)p;
    return NULL;
}

int nmx_vprintf(const char *fmt, va_list ap)
{
    IOLogv(fmt, ap);
    return 0;
}

/* ---- randomness and CRC-32 ---------------------------------------------------------------------------------------- */

void arc4random_buf(void *ptr, size_t len)
{
    while (len > 0) {
        u_int n = len > 1024 ? 1024 : (u_int)len;
        read_random(ptr, n);
        ptr = (char *)ptr + n;
        len -= n;
    }
}

uint32_t arc4random(void)
{
    uint32_t v;
    read_random(&v, sizeof(v));
    return v;
}

uint32_t arc4random_uniform(uint32_t upper_bound)
{
    if (upper_bound < 2)
        return 0;
    /* rejection sampling: discard values from the incomplete top range so every result is equally likely */
    uint32_t min = (uint32_t)(-upper_bound) % upper_bound;
    uint32_t r;
    do
        r = arc4random();
    while (r < min);
    return r % upper_bound;
}

u_long nmx_random(void)
{
    return arc4random() & 0x7fffffff;
}

/* IEEE 802.3 CRC-32 (reflected, polynomial 0xEDB88320), table built on first use */
static uint32_t crc32_tab[256];
static volatile int crc32_ready;

uint32_t crc32_raw(const void *buf, size_t size, uint32_t crc)
{
    if (!crc32_ready) {
        for (uint32_t i = 0; i < 256; i++) {
            uint32_t c = i;
            for (int k = 0; k < 8; k++)
                c = (c & 1) ? 0xEDB88320u ^ (c >> 1) : c >> 1;
            crc32_tab[i] = c;
        }
        __atomic_store_n(&crc32_ready, 1, __ATOMIC_RELEASE);
    }
    const uint8_t *p = buf;
    while (size--)
        crc = crc32_tab[(crc ^ *p++) & 0xff] ^ (crc >> 8);
    return crc;
}

/* ---- event handlers ----------------------------------------------------------------------------------------------- */

struct eh_entry {
    const char *name;
    void *func;
    void *arg;
    int priority;
    struct eh_entry *next;
};
static struct eh_entry *eh_list;
static struct mtx eh_mtx;
MTX_SYSINIT(eventhandler, &eh_mtx, "eventhandler", MTX_DEF);

eventhandler_tag nmx_eventhandler_register(const char *name, void *func, void *arg, int priority)
{
    struct eh_entry *e = malloc(sizeof(*e), M_NMXMISC, M_WAITOK | M_ZERO);
    if (e == NULL)
        return NULL;
    e->name = name;
    e->func = func;
    e->arg = arg;
    e->priority = priority;
    mtx_lock(&eh_mtx);
    e->next = eh_list;
    eh_list = e;
    mtx_unlock(&eh_mtx);
    return e;
}

void nmx_eventhandler_deregister(const char *name, eventhandler_tag tag)
{
    mtx_lock(&eh_mtx);
    for (struct eh_entry **pp = &eh_list; *pp; pp = &(*pp)->next) {
        if (*pp == tag) {
            struct eh_entry *e = *pp;
            *pp = e->next;
            mtx_unlock(&eh_mtx);
            free(e, M_NMXMISC);
            return;
        }
    }
    mtx_unlock(&eh_mtx);
}

/* calls every handler registered under 'name' as void (*)(void *arg, int how) */
void nmx_eventhandler_invoke(const char *name, int how)
{
    mtx_lock(&eh_mtx);
    for (struct eh_entry *e = eh_list; e; e = e->next)
        if (strcmp(e->name, name) == 0)
            ((void (*)(void *, int))e->func)(e->arg, how);
    mtx_unlock(&eh_mtx);
}

/* ---- notifications that have no macOS counterpart: logged so they are never silent -------------------------------- */

void devctl_notify(const char *system, const char *subsystem, const char *type, const char *data)
{
    printf("NullMoth xkpi: event %s.%s.%s %s\n", system ? system : "", subsystem ? subsystem : "", type ? type : "",
           data ? data : "");
}

void shutdown_nice(int howto)
{
    printf("NullMoth xkpi: the GPU driver asked for a shutdown (howto 0x%x); macOS power management decides\n", howto);
}

void kern_reboot(int howto)
{
    panic("NullMoth xkpi: kern_reboot(0x%x) requested by the GPU driver", howto);
}

int power_profile_get_state(void)
{
    return POWER_PROFILE_PERFORMANCE;
}

void swap_pager_status(int *total, int *used)
{
    *total = 0;
    *used = 0;
}

/* device interrupts are placed by xnu; an affinity request is accepted as done */
int intr_setaffinity(int irq, int mode, const void *mask)
{
    return 0;
}

/* ---- CPUs ----------------------------------------------------------------------------------------------------------- */

struct rendezvous_args {
    void (*setup)(void *);
    void (*action)(void *);
    void (*teardown)(void *);
    void *arg;
};

static void rendezvous_action(void *p)
{
    struct rendezvous_args *a = p;
    if (a->setup)
        a->setup(a->arg);
    if (a->action)
        a->action(a->arg);
    if (a->teardown)
        a->teardown(a->arg);
}

/* runs the action on every CPU with interrupts off (xnu's mp_rendezvous_no_intrs), as FreeBSD's smp_rendezvous does */
void smp_rendezvous(void (*setup)(void *), void (*action)(void *), void (*teardown)(void *), void *arg)
{
    struct rendezvous_args a = { setup, action, teardown, arg };
    mp_rendezvous_no_intrs(rendezvous_action, &a);
}

void smp_rendezvous_cpus(cpuset_t map, void (*setup)(void *), void (*action)(void *), void (*teardown)(void *), void *arg)
{
    smp_rendezvous(setup, action, teardown, arg);
}

/* ---- kernel threads ------------------------------------------------------------------------------------------------- */

struct kthread_start {
    void (*func)(void *);
    void *arg;
};

static void kthread_trampoline(void *p, wait_result_t wr)
{
    struct kthread_start ks = *(struct kthread_start *)p;
    free(p, M_NMXMISC);
    ks.func(ks.arg);
    kthread_exit();
}

int kthread_add(void (*func)(void *), void *arg, struct proc *p, struct thread **newtdp, int flags, int pages,
                const char *fmt, ...)
{
    struct kthread_start *ks = malloc(sizeof(*ks), M_NMXMISC, M_WAITOK);
    if (ks == NULL)
        return ENOMEM;
    ks->func = func;
    ks->arg = arg;
    thread_t th;
    if (kernel_thread_start(kthread_trampoline, ks, &th) != KERN_SUCCESS) {
        free(ks, M_NMXMISC);
        return ENOMEM;
    }
    char name[MAXCOMLEN + 1];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(name, sizeof(name), fmt, ap);
    va_end(ap);
    thread_set_thread_name(th, name);
    if (newtdp)
        *newtdp = NULL;     /* the record is created by the thread itself on first use */
    thread_deallocate(th);
    return 0;
}

int kproc_create(void (*func)(void *), void *arg, struct proc **newpp, int flags, int pages, const char *fmt, ...)
{
    if (newpp)
        *newpp = nmx_kernproc;
    return kthread_add(func, arg, NULL, NULL, flags, pages, "%s", fmt);
}

int kproc_kthread_add(void (*func)(void *), void *arg, struct proc **procptr, struct thread **tdptr, int flags,
                      int pages, const char *procname, const char *fmt, ...)
{
    if (procptr)
        *procptr = nmx_kernproc;
    return kthread_add(func, arg, NULL, tdptr, flags, pages, "%s", procname ? procname : "nmx");
}

void kthread_exit(void)
{
    nmx_thread_exit_cleanup();
    thread_terminate(current_thread());
    for (;;)
        IOSleep(1000);
}

int kthread_suspend(struct thread *td, int timo)
{
    return EOPNOTSUPP;
}

void kthread_suspend_check(void)
{
}

int kthread_resume(struct thread *td)
{
    return EOPNOTSUPP;
}

/* ---- privilege: privileged GPU operations require the superuser ---------------------------------------------------- */

int priv_check(struct thread *td, int priv)
{
    return kauth_cred_issuser(kauth_cred_get()) ? 0 : EPERM;
}

int priv_check_cred(struct ucred *cred, int priv)
{
    return kauth_cred_issuser(kauth_cred_get()) ? 0 : EPERM;
}

/* ---- the port's vnodes: reference counts only ---------------------------------------------------------------------- */

void vref(struct vnode *vp)
{
    __atomic_fetch_add(&vp->v_usecount, 1, __ATOMIC_SEQ_CST);
}

int vget(struct vnode *vp, int flags)
{
    vref(vp);
    return 0;
}

void vrele(struct vnode *vp)
{
    __atomic_fetch_sub(&vp->v_usecount, 1, __ATOMIC_SEQ_CST);
}

void vput(struct vnode *vp)
{
    vrele(vp);
}

/* ---- file descriptors and select: not available to kexts on macOS --------------------------------------------------- */

cap_rights_t cap_no_rights;

int fget(struct thread *td, int fd, cap_rights_t *rightsp, struct file **fpp)
{
    *fpp = NULL;
    return EBADF;
}

int fget_unlocked(struct thread *td, int fd, cap_rights_t *needrightsp, struct file **fpp)
{
    *fpp = NULL;
    return EBADF;
}

void fdrop(struct file *fp, struct thread *td)
{
    if (fp)
        __atomic_fetch_sub(&fp->f_count, 1, __ATOMIC_SEQ_CST);
}

int falloc(struct thread *td, struct file **resultfp, int *resultfd, int flags)
{
    return EOPNOTSUPP;
}

void finit(struct file *fp, u_int flag, short type, void *data, struct fileops *ops)
{
    fp->f_flag = flag;
    fp->f_type = type;
    fp->f_data = data;
    fp->f_ops = ops;
}

void fdclose(struct thread *td, struct file *fp, int idx)
{
}

/* poll() on a DRM node is wired through the device-node glue; until then these keep their state consistent */
void nmx_selrecord(struct thread *selector, struct selinfo *sip)
{
}

void nmx_selwakeup(struct selinfo *sip)
{
}

void nmx_seldrain(struct selinfo *sip)
{
}

void knlist_init(struct knlist *knl, void *lock, void (*kl_lock)(void *), void (*kl_unlock)(void *),
                 void (*kl_assert_lock)(void *, int))
{
    SLIST_INIT(&knl->kl_list);
    knl->kl_lockarg = lock;
    knl->kl_lock = kl_lock;
    knl->kl_unlock = kl_unlock;
    knl->kl_assert_lock = kl_assert_lock;
}

void knlist_destroy(struct knlist *knl)
{
}

void knlist_clear(struct knlist *knl, int islocked)
{
    SLIST_INIT(&knl->kl_list);
}
