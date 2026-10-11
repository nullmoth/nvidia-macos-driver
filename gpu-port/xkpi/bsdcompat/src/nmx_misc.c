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
#include <sys/counter.h>
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

/* FreeBSD uiomove for the kernel-buffer uios the device-node glue builds */
int nmx_uiomove(void *cp, int n, struct uio *uio)
{
    char *c = cp;
    while (n > 0 && uio->uio_resid > 0 && uio->uio_iovcnt > 0) {
        struct iovec *iov = uio->uio_iov;
        size_t cnt = iov->iov_len;
        if (cnt == 0) {
            uio->uio_iov++;
            uio->uio_iovcnt--;
            continue;
        }
        if (cnt > (size_t)n)
            cnt = (size_t)n;
        if (uio->uio_segflg != UIO_SYSSPACE)
            return EFAULT;
        if (uio->uio_rw == UIO_READ)
            memcpy(iov->iov_base, c, cnt);
        else
            memcpy(c, iov->iov_base, cnt);
        iov->iov_base = (char *)iov->iov_base + cnt;
        iov->iov_len -= cnt;
        uio->uio_resid -= (ssize_t)cnt;
        uio->uio_offset += (off_t)cnt;
        c += cnt;
        n -= (int)cnt;
    }
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

/*
 * Calls every handler registered under 'name' as void (*)(void *arg, int how), lowest priority value first (FreeBSD's
 * order), outside the list lock so a handler may sleep. Handlers are deregistered only at unload, never while an
 * event is being delivered.
 */
#define EH_MAX 32
void nmx_eventhandler_invoke(const char *name, int how)
{
    struct { void *func; void *arg; int pri; } h[EH_MAX];
    int n = 0;
    mtx_lock(&eh_mtx);
    for (struct eh_entry *e = eh_list; e && n < EH_MAX; e = e->next)
        if (strcmp(e->name, name) == 0) {
            int i = n++;
            while (i > 0 && h[i - 1].pri > e->priority) {
                h[i] = h[i - 1];
                i--;
            }
            h[i].func = e->func;
            h[i].arg = e->arg;
            h[i].pri = e->priority;
        }
    mtx_unlock(&eh_mtx);
    for (int i = 0; i < n; i++)
        ((void (*)(void *, int))h[i].func)(h[i].arg, how);
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

/* fdrop lives with the device nodes (nmx_linux_cdev.c): the last reference closes the file */

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

counter_u64_t counter_u64_alloc(int flags)
{
    return malloc(sizeof(uint64_t), M_NMXMISC, (flags & M_NOWAIT ? M_NOWAIT : M_WAITOK) | M_ZERO);
}

void counter_u64_free(counter_u64_t c)
{
    free(c, M_NMXMISC);
}

/* the "invalid operation" file entries, with FreeBSD kern_descrip.c's answers */
int invfo_rdwr(struct file *fp, struct uio *uio, struct ucred *cred, int flags, struct thread *td)
{
    return EOPNOTSUPP;
}

int invfo_truncate(struct file *fp, off_t length, struct ucred *cred, struct thread *td)
{
    return EINVAL;
}

int invfo_ioctl(struct file *fp, u_long com, void *data, struct ucred *cred, struct thread *td)
{
    return ENOTTY;
}

int invfo_poll(struct file *fp, int events, struct ucred *cred, struct thread *td)
{
    return 0;
}

int invfo_kqfilter(struct file *fp, struct knote *kn)
{
    return EINVAL;
}

int invfo_chmod(struct file *fp, mode_t mode, struct ucred *cred, struct thread *td)
{
    return EINVAL;
}

int invfo_chown(struct file *fp, uid_t uid, gid_t gid, struct ucred *cred, struct thread *td)
{
    return EINVAL;
}

int invfo_sendfile(struct file *fp, int sockfd, struct uio *hdr_uio, struct uio *trl_uio, off_t offset, size_t nbytes,
                   off_t *sent, int flags, struct thread *td)
{
    return EINVAL;
}

/*
 * F_SETOWN / FIOSETOWN: xnu gives kexts no way to post SIGIO to a process group on a device's behalf, so a request
 * for an owner is refused (EOPNOTSUPP) rather than accepted and then never signalled. Clearing the owner (0) works.
 */
int fsetown(pid_t pgid, struct sigio **sigiop)
{
    if (pgid == 0) {
        *sigiop = NULL;
        return 0;
    }
    return EOPNOTSUPP;
}

pid_t fgetown(struct sigio **sigiop)
{
    return 0;
}

void funsetown(struct sigio **sigiop)
{
    *sigiop = NULL;
}

/*
 * libkern functions xnu declares in its headers but does not export to kexts (a kmutil link of the kext names
 * them): fls, flsll, strnstr, qsort, strtoll, strtoull, suword64.
 */
int fls(unsigned int x)
{
    return x ? 32 - __builtin_clz(x) : 0;
}

int flsll(unsigned long long x)
{
    return x ? 64 - __builtin_clzll(x) : 0;
}

char *strnstr(const char *s, const char *find, size_t slen)
{
    size_t len = strlen(find);
    if (len == 0)
        return __DECONST(char *, s);
    for (; slen >= len && *s; s++, slen--)
        if (*s == *find && strncmp(s, find, len) == 0)
            return __DECONST(char *, s);
    return NULL;
}

static int digit_value(int c)
{
    if (c >= '0' && c <= '9')
        return c - '0';
    if (c >= 'a' && c <= 'z')
        return c - 'a' + 10;
    if (c >= 'A' && c <= 'Z')
        return c - 'A' + 10;
    return 99;
}

/* the C library's rules: leading space, sign, 0x / 0 prefixes for base 16 / 0, saturation on overflow */
static unsigned long long parse_ull(const char *nptr, char **endptr, int base, int *neg, int *overflow)
{
    const char *s = nptr;
    *neg = 0;
    *overflow = 0;
    while (*s == ' ' || (*s >= '\t' && *s <= '\r'))
        s++;
    if (*s == '-' || *s == '+')
        *neg = *s++ == '-';
    if ((base == 0 || base == 16) && s[0] == '0' && (s[1] == 'x' || s[1] == 'X') && digit_value(s[2]) < 16) {
        s += 2;
        base = 16;
    } else if (base == 0) {
        base = s[0] == '0' ? 8 : 10;
    }
    if (base < 2 || base > 36) {
        if (endptr)
            *endptr = __DECONST(char *, nptr);
        return 0;
    }
    unsigned long long acc = 0;
    int any = 0;
    for (;; s++) {
        int d = digit_value((unsigned char)*s);
        if (d >= base)
            break;
        any = 1;
        if (acc > (~0ULL - (unsigned)d) / (unsigned)base)
            *overflow = 1;
        else
            acc = acc * (unsigned)base + (unsigned)d;
    }
    if (endptr)
        *endptr = __DECONST(char *, any ? s : nptr);
    return acc;
}

unsigned long long strtoull(const char *nptr, char **endptr, int base)
{
    int neg, overflow;
    unsigned long long v = parse_ull(nptr, endptr, base, &neg, &overflow);
    if (overflow)
        return ~0ULL;
    return neg ? (unsigned long long)(-(long long)v) : v;
}

long long strtoll(const char *nptr, char **endptr, int base)
{
    int neg, overflow;
    unsigned long long v = parse_ull(nptr, endptr, base, &neg, &overflow);
    const unsigned long long lim = neg ? (unsigned long long)LLONG_MAX + 1 : (unsigned long long)LLONG_MAX;
    if (overflow || v > lim)
        return neg ? LLONG_MIN : LLONG_MAX;
    return neg ? (long long)(0 - v) : (long long)v;
}

int suword64(volatile void *base, int64_t word)
{
    return copyout(&word, __DEVOLATILE(void *, base), sizeof(word)) == 0 ? 0 : -1;
}

void *bsearch(const void *key, const void *base, size_t n, size_t size, int (*cmp)(const void *, const void *))
{
    const char *lo = base;
    while (n > 0) {
        const char *mid = lo + (n / 2) * size;
        int c = cmp(key, mid);
        if (c == 0)
            return __DECONST(void *, mid);
        if (c > 0) {
            lo = mid + size;
            n = n - n / 2 - 1;
        } else {
            n /= 2;
        }
    }
    return NULL;
}

/*
 * pagefault_disable(): copies from user memory must not sleep on a fault while this is set (LinuxKPI's
 * copy_from_user_inatomic then returns a short count and the driver takes its slow path)
 */
int vm_fault_disable_pagefaults(void)
{
    struct thread *td = curthread;
    int save = td->td_pflags & TDP_NOFAULTING;
    td->td_pflags |= TDP_NOFAULTING;
    return save;
}

void vm_fault_enable_pagefaults(int save)
{
    struct thread *td = curthread;
    td->td_pflags = (td->td_pflags & ~TDP_NOFAULTING) | save;
}

/*
 * copyin/copyout for code running with page faults disabled (LinuxKPI's __copy_from_user_inatomic). xnu has no
 * fault-free user copy for kexts; its copyin may take and resolve a fault. The callers (i915 relocations, amdgpu)
 * hold sleepable locks only, never a spin lock, so a resolved fault is safe; a failed one returns EFAULT and the
 * driver takes its slow path, as on Linux.
 */
int nmx_copyin_nofault(const void *uaddr, void *kaddr, size_t len)
{
    return copyin(uaddr, kaddr, len);
}

int nmx_copyout_nofault(const void *kaddr, void *uaddr, size_t len)
{
    return copyout(kaddr, uaddr, len);
}

static int qsort_adapter(const void *a, const void *b, void *cmp)
{
    return ((int (*)(const void *, const void *))cmp)(a, b);
}

void qsort(void *a, size_t n, size_t es, int (*cmp)(const void *, const void *))
{
    qsort_r(a, n, es, qsort_adapter, (void *)cmp);
}

/*
 * qsort_r (C11 argument order, as FreeBSD 14 and LinuxKPI's list_sort use it). Heapsort: no recursion and no
 * allocation, so it is safe at any kernel stack depth, and O(n log n) in the worst case.
 */
static void qs_swap(char *a, char *b, size_t size)
{
    while (size--) {
        char t = *a;
        *a++ = *b;
        *b++ = t;
    }
}

static void qs_sift(char *base, size_t root, size_t n, size_t size, int (*cmp)(const void *, const void *, void *),
                    void *thunk)
{
    for (;;) {
        size_t child = 2 * root + 1;
        if (child >= n)
            return;
        if (child + 1 < n && cmp(base + child * size, base + (child + 1) * size, thunk) < 0)
            child++;
        if (cmp(base + root * size, base + child * size, thunk) >= 0)
            return;
        qs_swap(base + root * size, base + child * size, size);
        root = child;
    }
}

void qsort_r(void *a, size_t n, size_t size, int (*cmp)(const void *, const void *, void *), void *thunk)
{
    char *base = a;
    if (n < 2 || size == 0)
        return;
    for (size_t i = n / 2; i-- > 0;)
        qs_sift(base, i, n, size, cmp, thunk);
    for (size_t end = n - 1; end > 0; end--) {
        qs_swap(base, base + end * size, size);
        qs_sift(base, 0, end, size, cmp, thunk);
    }
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
