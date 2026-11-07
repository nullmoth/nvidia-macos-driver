/*
 * lnxlibc.c - glibc (x86-64) entry points whose ABI differs from macOS libc, translated.
 *
 * elfhost binds an import here before falling back to the same-named macOS function. Each shim names the
 * difference it covers. Layouts are the Linux x86-64 ones (glibc bits headers, kernel uapi), written out by hand.
 */
#include "lnxlibc.h"

#include <dirent.h>
#include <errno.h>
#include <fenv.h>
#include <iconv.h>
#include <pthread.h>
#include <pwd.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/sysctl.h>
#include <sys/utsname.h>
#include <time.h>
#include <unistd.h>

static int g_trace_l = -1;
#define LLOG(...) do { if (g_trace_l < 0) g_trace_l = getenv("ELFHOST_TRACE") != NULL; \
                       if (g_trace_l) fprintf(stderr, "[lnxlibc] " __VA_ARGS__); } while (0)

/* ---- address -> macOS object map for Linux-sized synchronisation objects ----------------------- */
typedef struct ent { const void *key; void *obj; struct ent *next; } ent;
static ent *g_map[8192];
static pthread_mutex_t g_map_lock = PTHREAD_MUTEX_INITIALIZER;
/* One lookup; the object is created on first use. initk, when given, may read the Linux object's own
 * bytes (a statically initialised glibc mutex carries its kind there). */
static void *obj_get(const void *k, size_t size, void (*init)(void *), void (*initk)(void *, const void *)) {
    size_t h = ((uintptr_t)k >> 3) & 8191;
    pthread_mutex_lock(&g_map_lock);
    ent *e = g_map[h];
    while (e && e->key != k) e = e->next;
    if (!e) {
        e = malloc(sizeof *e); e->key = k; e->obj = calloc(1, size);
        if (initk) initk(e->obj, k); else if (init) init(e->obj);
        e->next = g_map[h]; g_map[h] = e;
    }
    pthread_mutex_unlock(&g_map_lock);
    return e->obj;
}
void *lnx_obj(const void *k, size_t size, void (*init)(void *)) { return obj_get(k, size, init, NULL); }
static void lnx_obj_drop(const void *k) {
    size_t h = ((uintptr_t)k >> 3) & 8191;
    pthread_mutex_lock(&g_map_lock);
    for (ent **pp = &g_map[h]; *pp; pp = &(*pp)->next)
        if ((*pp)->key == k) { ent *e = *pp; *pp = e->next; free(e->obj); free(e); break; }
    pthread_mutex_unlock(&g_map_lock);
}

/* ---- pthread: mutex 40 B, mutexattr 4 B, cond 48 B (zero = static init), attr 56 B, key 4 B -------- */
/* glibc's static PTHREAD_RECURSIVE_MUTEX_INITIALIZER_NP sets __kind (int at offset 16) to 1. Treating it as
 * a normal mutex deadlocks the JIT linker's constructors on their first re-entrant lock (the load hangs in
 * _pthread_mutex_firstfit_lock_wait). Kind 2 = error-checking. */
static void mtx_from_linux(void *p, const void *k) {
    int kind = ((const int *)k)[4] & 3;
    pthread_mutexattr_t a; pthread_mutexattr_init(&a);
    pthread_mutexattr_settype(&a, kind == 1 ? PTHREAD_MUTEX_RECURSIVE : kind == 2 ? PTHREAD_MUTEX_ERRORCHECK : PTHREAD_MUTEX_NORMAL);
    pthread_mutex_init(p, &a); pthread_mutexattr_destroy(&a);
}
static void cond_default(void *p) { pthread_cond_init(p, NULL); }
static void rw_default(void *p) { pthread_rwlock_init(p, NULL); }
static void mattr_default(void *p) { pthread_mutexattr_init(p); }
/* Linux threads default to an 8 MiB stack, macOS secondary threads to 512 KiB. A compiler recursing on
 * a big kernel would overflow the macOS default, so every thread we create gets the Linux size. */
static void tattr_default(void *p) { pthread_attr_init(p); pthread_attr_setstacksize(p, 8u << 20); }

#define MTX(m) ((pthread_mutex_t *)obj_get((m), sizeof(pthread_mutex_t), NULL, mtx_from_linux))
#define CND(c) ((pthread_cond_t *)lnx_obj((c), sizeof(pthread_cond_t), cond_default))
#define RWL(l) ((pthread_rwlock_t *)lnx_obj((l), sizeof(pthread_rwlock_t), rw_default))
#define MAT(a) ((pthread_mutexattr_t *)lnx_obj((a), sizeof(pthread_mutexattr_t), mattr_default))
#define TAT(a) ((pthread_attr_t *)lnx_obj((a), sizeof(pthread_attr_t), tattr_default))

static int s_mutexattr_init(void *a) { (void)MAT(a); return 0; }
static int s_mutexattr_destroy(void *a) { lnx_obj_drop(a); return 0; }
static int s_mutexattr_settype(void *a, int t) {
    /* Linux NORMAL 0, RECURSIVE 1, ERRORCHECK 2; macOS NORMAL 0, ERRORCHECK 1, RECURSIVE 2 */
    int mt = t == 1 ? PTHREAD_MUTEX_RECURSIVE : t == 2 ? PTHREAD_MUTEX_ERRORCHECK : PTHREAD_MUTEX_NORMAL;
    return pthread_mutexattr_settype(MAT(a), mt);
}
static int s_mutex_init(void *m, void *a) {
    pthread_mutex_t *o = MTX(m); pthread_mutex_destroy(o);
    return pthread_mutex_init(o, a ? MAT(a) : NULL);
}
static int s_mutex_destroy(void *m) { pthread_mutex_destroy(MTX(m)); lnx_obj_drop(m); return 0; }
static int s_mutex_lock(void *m) { return pthread_mutex_lock(MTX(m)); }
static int s_mutex_unlock(void *m) { return pthread_mutex_unlock(MTX(m)); }
static int s_mutex_trylock(void *m) { return pthread_mutex_trylock(MTX(m)) == EBUSY ? 16 : 0; }   /* Linux EBUSY 16 */
static int s_cond_init(void *c, void *a) { (void)a; pthread_cond_t *o = CND(c); pthread_cond_destroy(o); return pthread_cond_init(o, NULL); }
static int s_cond_destroy(void *c) { pthread_cond_destroy(CND(c)); lnx_obj_drop(c); return 0; }
static int s_cond_signal(void *c) { return pthread_cond_signal(CND(c)); }
static int s_cond_broadcast(void *c) { return pthread_cond_broadcast(CND(c)); }
static int s_cond_wait(void *c, void *m) { return pthread_cond_wait(CND(c), MTX(m)); }
static int s_cond_timedwait(void *c, void *m, const struct timespec *t) {
    int r = pthread_cond_timedwait(CND(c), MTX(m), t);
    return r == ETIMEDOUT ? 110 : r;   /* Linux ETIMEDOUT 110 */
}
static int s_rw_init(void *l, void *a) { (void)a; (void)RWL(l); return 0; }
static int s_rw_destroy(void *l) { pthread_rwlock_destroy(RWL(l)); lnx_obj_drop(l); return 0; }
static int s_rw_rd(void *l) { return pthread_rwlock_rdlock(RWL(l)); }
static int s_rw_wr(void *l) { return pthread_rwlock_wrlock(RWL(l)); }
static int s_rw_un(void *l) { return pthread_rwlock_unlock(RWL(l)); }

static int s_attr_init(void *a) { (void)TAT(a); return 0; }
static int s_attr_destroy(void *a) { lnx_obj_drop(a); return 0; }
static int s_attr_setstacksize(void *a, size_t s) { return pthread_attr_setstacksize(TAT(a), s < (8u << 20) ? (8u << 20) : s); }
static int s_attr_setdetachstate(void *a, int d) {
    /* Linux JOINABLE 0, DETACHED 1; macOS JOINABLE 1, DETACHED 2 */
    return pthread_attr_setdetachstate(TAT(a), d ? PTHREAD_CREATE_DETACHED : PTHREAD_CREATE_JOINABLE);
}
static int s_attr_sched_noop(void *a, long v) { (void)a; (void)v; return 0; }   /* scheduling hints: ignored */
static int s_pthread_create(pthread_t *t, void *a, void *(*fn)(void *), void *arg) {
    if (a) return pthread_create(t, TAT(a), fn, arg);
    pthread_attr_t d; tattr_default(&d); int r = pthread_create(t, &d, fn, arg); pthread_attr_destroy(&d); return r;
}
/* keys are 4 bytes on Linux; the callee may see garbage above bit 31 of the register, so take unsigned */
static int s_key_create(unsigned *k, void (*d)(void *)) { pthread_key_t mk; int r = pthread_key_create(&mk, d); if (!r) *k = (unsigned)mk; return r; }
static int s_key_delete(unsigned k) { return pthread_key_delete((pthread_key_t)k); }
static void *s_getspecific(unsigned k) { return pthread_getspecific((pthread_key_t)k); }
static int s_setspecific(unsigned k, const void *v) { return pthread_setspecific((pthread_key_t)k, v); }

/* signals: sets are 128 B on Linux, 4 B on macOS, and numbers differ. The objects only install/restore
 * masks and crash handlers around their own work, so these are accepted and not applied (logged). */
static int s_sigset_fill(void *s) { memset(s, 0xff, 128); return 0; }
static int s_sigset_empty(void *s) { memset(s, 0, 128); return 0; }
static int s_sigmask(int how, const void *set, void *old) { (void)how; (void)set; if (old) memset(old, 0, 128); return 0; }
static int s_sigaction(int sig, const void *act, void *old) { (void)act; LLOG("sigaction(%d) not applied\n", sig); if (old) memset(old, 0, 152); return 0; }
static int s_pthread_kill(pthread_t t, int sig) { if (sig == 0) return pthread_kill(t, 0); LLOG("pthread_kill sig %d not sent\n", sig); return 0; }

/* unnamed semaphores: macOS has none (sem_init returns ENOSYS), so build them */
typedef struct { pthread_mutex_t m; pthread_cond_t c; unsigned v; } lsem;
static void sem_default(void *p) { lsem *s = p; pthread_mutex_init(&s->m, NULL); pthread_cond_init(&s->c, NULL); }
#define SEM(s) ((lsem *)lnx_obj((s), sizeof(lsem), sem_default))
static int s_sem_init(void *s, int ps, unsigned v) { (void)ps; SEM(s)->v = v; return 0; }
static int s_sem_destroy(void *s) { lnx_obj_drop(s); return 0; }
static int s_sem_post(void *s) { lsem *x = SEM(s); pthread_mutex_lock(&x->m); x->v++; pthread_cond_signal(&x->c); pthread_mutex_unlock(&x->m); return 0; }
static int s_sem_wait(void *s) { lsem *x = SEM(s); pthread_mutex_lock(&x->m); while (!x->v) pthread_cond_wait(&x->c, &x->m); x->v--; pthread_mutex_unlock(&x->m); return 0; }
static int s_sem_trywait(void *s) {
    lsem *x = SEM(s); int r = 0; pthread_mutex_lock(&x->m);
    if (x->v) x->v--; else { errno = EAGAIN; r = -1; }
    pthread_mutex_unlock(&x->m); return r;
}

/* ---- CPU count / affinity ------------------------------------------------------------------- */
static int ncpu(void) { int n = 1; size_t l = sizeof n; sysctlbyname("hw.logicalcpu", &n, &l, NULL, 0); return n > 0 ? n : 1; }
static int s_get_nprocs(void) { return ncpu(); }
static int s_sched_getaffinity(int pid, size_t sz, uint8_t *mask) {
    (void)pid; memset(mask, 0, sz); int n = ncpu();
    for (int i = 0; i < n && (size_t)(i / 8) < sz; i++) mask[i / 8] |= 1u << (i % 8);
    return 0;
}
static int s_sched_cpucount(size_t sz, const uint8_t *set) { int c = 0; for (size_t i = 0; i < sz; i++) c += __builtin_popcount(set[i]); return c; }

/* ---- sysconf: names differ ---------------------------------------------------------------------- */
static long s_sysconf(int n) {
    switch (n) {
    case 0: return sysconf(_SC_ARG_MAX);       case 2: return sysconf(_SC_CLK_TCK);
    case 4: return sysconf(_SC_OPEN_MAX);      case 30: return sysconf(_SC_PAGESIZE);
    case 83: return sysconf(_SC_NPROCESSORS_CONF); case 84: return sysconf(_SC_NPROCESSORS_ONLN);
    case 85: return sysconf(_SC_PHYS_PAGES);
    default: LLOG("sysconf(%d) unknown -> EINVAL\n", n); errno = EINVAL; return -1;
    }
}

/* ---- mmap: anonymous / noreserve flags differ ------------------------------------------------- */
static void *s_mmap(void *a, size_t l, int prot, int fl, int fd, off_t off) {
    int m = fl & (MAP_SHARED | MAP_PRIVATE | MAP_FIXED);
    if (fl & 0x20) m |= MAP_ANON;
    if (fl & 0x4000) m |= MAP_NORESERVE;
    return mmap(a, l, prot, m, fd, off);
}

/* ---- stat family: glibc's versioned __xstat with the Linux 144-byte struct stat ---------------- */
typedef struct { uint64_t dev, ino, nlink; uint32_t mode, uid, gid, pad0; uint64_t rdev; int64_t size, blksize, blocks;
                 struct timespec atim, mtim, ctim; int64_t reserved[3]; } lstat_t;
static int to_lstat(int r, const struct stat *s, lstat_t *o) {
    if (r) return r;
    memset(o, 0, sizeof *o);
    o->dev = s->st_dev; o->ino = s->st_ino; o->nlink = s->st_nlink; o->mode = s->st_mode;   /* S_IF* bits agree */
    o->uid = s->st_uid; o->gid = s->st_gid; o->rdev = s->st_rdev; o->size = s->st_size;
    o->blksize = s->st_blksize; o->blocks = s->st_blocks;
    o->atim = s->st_atimespec; o->mtim = s->st_mtimespec; o->ctim = s->st_ctimespec;
    return 0;
}
static int s_xstat(int v, const char *p, lstat_t *o) { (void)v; struct stat s; LLOG("stat(%s)\n", p); return to_lstat(stat(p, &s), &s, o); }
static int s_lxstat(int v, const char *p, lstat_t *o) { (void)v; struct stat s; LLOG("stat(%s)\n", p); return to_lstat(lstat(p, &s), &s, o); }
static int s_fxstat(int v, int fd, lstat_t *o) { (void)v; struct stat s; return to_lstat(fstat(fd, &s), &s, o); }

typedef struct { int64_t type, bsize; uint64_t blocks, bfree, bavail, files, ffree; int32_t fsid[2];
                 int64_t namelen, frsize, flags, spare[4]; } lstatfs_t;
static int s_statfs(const char *p, lstatfs_t *o) {
    struct statfs s; if (statfs(p, &s)) return -1;
    memset(o, 0, sizeof *o);
    o->bsize = s.f_bsize; o->frsize = s.f_bsize; o->blocks = s.f_blocks; o->bfree = s.f_bfree; o->bavail = s.f_bavail;
    o->files = s.f_files; o->ffree = s.f_ffree; o->namelen = 255;
    o->type = 0x01021994;   /* report tmpfs: a local, writable filesystem */
    return 0;
}

/* ---- readdir: Linux struct dirent is {ino, off, reclen, type, name[256]} ----------------------- */
typedef struct { uint64_t ino; int64_t off; uint16_t reclen; uint8_t type; char name[256]; } ldirent;
static void *s_readdir(DIR *d) {
    static _Thread_local ldirent out;
    struct dirent *e = readdir(d);
    if (!e) return NULL;
    out.ino = e->d_ino; out.off = 0; out.reclen = sizeof out; out.type = e->d_type;   /* DT_* agree */
    snprintf(out.name, sizeof out.name, "%s", e->d_name);
    return &out;
}

/* ---- passwd: Linux {name, passwd, uid, gid, gecos, dir, shell} ----------------------------------- */
typedef struct { char *name, *passwd; uint32_t uid, gid; char *gecos, *dir, *shell; } lpasswd;
static int pw_copy(struct passwd *m, lpasswd *o, char *buf, size_t n, lpasswd **res) {
    char *p = buf, *e = buf + n;
    const char *src[5] = { m->pw_name, m->pw_passwd, m->pw_gecos, m->pw_dir, m->pw_shell };
    char **dst[5] = { &o->name, &o->passwd, &o->gecos, &o->dir, &o->shell };
    for (int i = 0; i < 5; i++) {
        size_t l = strlen(src[i] ? src[i] : "") + 1;
        if (p + l > e) { *res = NULL; return 34; }   /* Linux ERANGE 34 */
        memcpy(p, src[i] ? src[i] : "", l); *dst[i] = p; p += l;
    }
    o->uid = m->pw_uid; o->gid = m->pw_gid; *res = o; return 0;
}
static int s_getpwuid_r(uint32_t uid, lpasswd *o, char *buf, size_t n, lpasswd **res) {
    struct passwd mp, *mr; char mb[4096];
    if (getpwuid_r(uid, &mp, mb, sizeof mb, &mr) || !mr) { *res = NULL; return 0; }
    return pw_copy(mr, o, buf, n, res);
}
static int s_getpwnam_r(const char *nm, lpasswd *o, char *buf, size_t n, lpasswd **res) {
    struct passwd mp, *mr; char mb[4096];
    if (getpwnam_r(nm, &mp, mb, sizeof mb, &mr) || !mr) { *res = NULL; return 0; }
    return pw_copy(mr, o, buf, n, res);
}

/* ---- uname: Linux has six 65-byte fields --------------------------------------------------------- */
static int s_uname(char (*o)[65]) {
    struct utsname u; if (uname(&u)) return -1;
    memset(o, 0, 6 * 65);
    snprintf(o[0], 65, "Linux");  /* the object asks to pick code paths written for Linux */
    snprintf(o[1], 65, "%s", u.nodename); snprintf(o[2], 65, "6.0.0"); snprintf(o[3], 65, "%s", u.version);
    snprintf(o[4], 65, "x86_64");
    return 0;
}

/* ---- fenv: Linux fenv_t is 28 bytes, macOS 32; keep the real one beside the caller's ------------ */
static void fenv_default(void *p) { fegetenv(p); }
static int s_fegetenv(void *e) { return fegetenv(lnx_obj(e, sizeof(fenv_t), fenv_default)); }
static int s_fesetenv(const void *e) {
    if (e == (const void *)-1) return fesetenv(FE_DFL_ENV);     /* Linux FE_DFL_ENV */
    return fesetenv(lnx_obj(e, sizeof(fenv_t), fenv_default));
}

/* ---- misc --------------------------------------------------------------------------------------- */
typedef struct { int v[10]; } lmallinfo;
static lmallinfo s_mallinfo(void) { lmallinfo z; memset(&z, 0, sizeof z); return z; }
static char *s_strerror_r(int e, char *b, size_t n) { strerror_r(e, b, n); return b; }   /* GNU variant returns the text */
static int s_ioctl(int fd, unsigned long req, ...) { LLOG("ioctl(%d, 0x%lx) -> ENOTTY\n", fd, req); errno = ENOTTY; return -1; }


/* ---- glibc ctype tables: __ctype_b_loc / __ctype_tolower_loc return pointers into 384-entry tables
 * indexed -128..255. Bit values are glibc's little-endian _ISbit(): upper 0x100, lower 0x200, alpha 0x400,
 * digit 0x800, xdigit 0x1000, space 0x2000, print 0x4000, graph 0x8000, blank 0x1, cntrl 0x2, punct 0x4,
 * alnum 0x8. Built for the "C" locale, which is what these objects run in. */
#include <ctype.h>
static unsigned short g_ctb[384]; static int32_t g_ctl[384], g_ctu[384];
static const unsigned short *g_ctb_p; static const int32_t *g_ctl_p, *g_ctu_p;
static pthread_once_t g_ct_once = PTHREAD_ONCE_INIT;
static void ct_init(void) {
    for (int i = -128; i < 256; i++) {
        int c = i < 0 ? i + 256 : i; unsigned short v = 0;
        if (c < 128) {
            if (isupper(c)) v |= 0x100; if (islower(c)) v |= 0x200; if (isalpha(c)) v |= 0x400; if (isdigit(c)) v |= 0x800;
            if (isxdigit(c)) v |= 0x1000; if (isspace(c)) v |= 0x2000; if (isprint(c)) v |= 0x4000; if (isgraph(c)) v |= 0x8000;
            if (c == ' ' || c == '\t') v |= 0x1; if (iscntrl(c)) v |= 0x2; if (ispunct(c)) v |= 0x4; if (isalnum(c)) v |= 0x8;
        }
        g_ctb[i + 128] = i < 0 ? 0 : v;
        g_ctl[i + 128] = (i >= 0 && c < 128) ? tolower(c) : i;
        g_ctu[i + 128] = (i >= 0 && c < 128) ? toupper(c) : i;
    }
    g_ctb_p = g_ctb + 128; g_ctl_p = g_ctl + 128; g_ctu_p = g_ctu + 128;
}
static const unsigned short **s_ctype_b_loc(void) { pthread_once(&g_ct_once, ct_init); return &g_ctb_p; }
static const int32_t **s_ctype_tolower_loc(void) { pthread_once(&g_ct_once, ct_init); return &g_ctl_p; }
static const int32_t **s_ctype_toupper_loc(void) { pthread_once(&g_ct_once, ct_init); return &g_ctu_p; }

/* ---- network + SysV shm: refused. The NGX core uses them only for its updater/telemetry; nothing leaves the
 * machine from inside the driver. Every refusal is logged. */
static int s_net_refuse(void) { LLOG("network call refused\n"); errno = EACCES; return -1; }
static int s_getaddrinfo(const char *h, const char *sv, const void *hint, void **res) { (void)sv; (void)hint; *res = NULL; LLOG("getaddrinfo(%s) refused\n", h ? h : "-"); return -4; /* Linux EAI_FAIL */ }
static void s_freeaddrinfo(void *p) { (void)p; }
static const char *s_gai_strerror(int e) { (void)e; return "network disabled"; }
static void *s_gethostbyname(const char *n) { LLOG("gethostbyname(%s) refused\n", n); return NULL; }
static long s_shm_refuse(void) { LLOG("SysV shm refused\n"); errno = ENOSYS; return -1; }
static void *s_shmat(int id, const void *a, int f) { (void)id; (void)a; (void)f; errno = ENOSYS; return (void *)-1; }
static int s_tty_refuse(void) { errno = ENOTTY; return -1; }
static int s_mlock(const void *a, size_t n) { (void)a; (void)n; return 0; }
static void *s_signal(int sig, void *h) { (void)h; LLOG("signal(%d) not installed\n", sig); return NULL; }

/* fnmatch: Linux PATHNAME 1, NOESCAPE 2, PERIOD 4; macOS NOESCAPE 1, PATHNAME 2, PERIOD 4 */
#include <fnmatch.h>
static int s_fnmatch(const char *pat, const char *str, int fl) {
    int m = (fl & 1 ? FNM_PATHNAME : 0) | (fl & 2 ? FNM_NOESCAPE : 0) | (fl & 4 ? FNM_PERIOD : 0);
    return fnmatch(pat, str, m) ? 1 : 0;   /* Linux FNM_NOMATCH 1 */
}
static int s_xpg_strerror_r(int e, char *b, size_t n) { return strerror_r(e, b, n); }

/* scandir/alphasort over Linux dirents (layout in s_readdir above) */
static int s_alphasort(const ldirent **a, const ldirent **b) { return strcoll((*a)->name, (*b)->name); }
static int s_scandir(const char *dir, ldirent ***out, int (*filt)(const ldirent *), int (*cmp)(const ldirent **, const ldirent **)) {
    DIR *d = opendir(dir); if (!d) return -1;
    size_t n = 0, cap = 16; ldirent **v = malloc(cap * sizeof *v); struct dirent *e;
    while ((e = readdir(d))) {
        ldirent t = { e->d_ino, 0, sizeof(ldirent), e->d_type, {0} };
        snprintf(t.name, sizeof t.name, "%s", e->d_name);
        if (filt && !filt(&t)) continue;
        if (n == cap) v = realloc(v, (cap *= 2) * sizeof *v);
        v[n] = malloc(sizeof t); *v[n++] = t;
    }
    closedir(d);
    if (cmp) qsort(v, n, sizeof *v, (int (*)(const void *, const void *))cmp);
    *out = v; return (int)n;
}


/* traced file access (ELFHOST_TRACE): the objects decide things from which files exist */
static FILE *s_fopen(const char *p, const char *m) { FILE *f = fopen(p, m); LLOG("fopen(%s, %s) -> %s\n", p, m, f ? "ok" : "fail"); return f; }
static int s_access(const char *p, int m) { int r = access(p, m); LLOG("access(%s, %d) -> %d\n", p, m, r); return r; }
static DIR *s_opendir(const char *p) { DIR *d = opendir(p); LLOG("opendir(%s) -> %s\n", p, d ? "ok" : "fail"); return d; }

static const struct { const char *name; void *addr; } g_lnx[] = {
    {"pthread_mutexattr_init", (void *)s_mutexattr_init}, {"pthread_mutexattr_destroy", (void *)s_mutexattr_destroy},
    {"pthread_mutexattr_settype", (void *)s_mutexattr_settype},
    {"pthread_mutex_init", (void *)s_mutex_init}, {"pthread_mutex_destroy", (void *)s_mutex_destroy},
    {"pthread_mutex_lock", (void *)s_mutex_lock}, {"pthread_mutex_unlock", (void *)s_mutex_unlock},
    {"pthread_mutex_trylock", (void *)s_mutex_trylock},
    {"pthread_cond_init", (void *)s_cond_init}, {"pthread_cond_destroy", (void *)s_cond_destroy},
    {"pthread_cond_signal", (void *)s_cond_signal}, {"pthread_cond_broadcast", (void *)s_cond_broadcast},
    {"pthread_cond_wait", (void *)s_cond_wait}, {"pthread_cond_timedwait", (void *)s_cond_timedwait},
    {"pthread_rwlock_init", (void *)s_rw_init}, {"pthread_rwlock_destroy", (void *)s_rw_destroy},
    {"pthread_rwlock_rdlock", (void *)s_rw_rd}, {"pthread_rwlock_wrlock", (void *)s_rw_wr},
    {"pthread_rwlock_unlock", (void *)s_rw_un},
    {"pthread_attr_init", (void *)s_attr_init}, {"pthread_attr_destroy", (void *)s_attr_destroy},
    {"pthread_attr_setstacksize", (void *)s_attr_setstacksize}, {"pthread_attr_setdetachstate", (void *)s_attr_setdetachstate},
    {"pthread_attr_setinheritsched", (void *)s_attr_sched_noop}, {"pthread_attr_setschedparam", (void *)s_attr_sched_noop},
    {"pthread_attr_setschedpolicy", (void *)s_attr_sched_noop},
    {"pthread_create", (void *)s_pthread_create},
    {"pthread_key_create", (void *)s_key_create}, {"__pthread_key_create", (void *)s_key_create},
    {"pthread_key_delete", (void *)s_key_delete}, {"pthread_getspecific", (void *)s_getspecific},
    {"pthread_setspecific", (void *)s_setspecific}, {"pthread_kill", (void *)s_pthread_kill},
    {"pthread_sigmask", (void *)s_sigmask}, {"sigprocmask", (void *)s_sigmask}, {"sigaction", (void *)s_sigaction},
    {"sigemptyset", (void *)s_sigset_empty}, {"sigfillset", (void *)s_sigset_fill},
    {"sem_init", (void *)s_sem_init}, {"sem_destroy", (void *)s_sem_destroy}, {"sem_post", (void *)s_sem_post},
    {"sem_wait", (void *)s_sem_wait}, {"sem_trywait", (void *)s_sem_trywait},
    {"get_nprocs", (void *)s_get_nprocs}, {"sched_getaffinity", (void *)s_sched_getaffinity},
    {"__sched_cpucount", (void *)s_sched_cpucount}, {"sysconf", (void *)s_sysconf}, {"mmap", (void *)s_mmap},
    {"__xstat", (void *)s_xstat}, {"__lxstat", (void *)s_lxstat}, {"__fxstat", (void *)s_fxstat},
    {"__fxstat64", (void *)s_fxstat}, {"statfs", (void *)s_statfs}, {"readdir", (void *)s_readdir},
    {"getpwuid_r", (void *)s_getpwuid_r}, {"getpwnam_r", (void *)s_getpwnam_r}, {"uname", (void *)s_uname},
    {"fegetenv", (void *)s_fegetenv}, {"fesetenv", (void *)s_fesetenv}, {"mallinfo", (void *)s_mallinfo},
    {"strerror_r", (void *)s_strerror_r}, {"ioctl", (void *)s_ioctl},
    {"__isoc99_sscanf", (void *)sscanf}, {"_IO_getc", (void *)getc}, {"_IO_putc", (void *)putc},
    {"__strtok_r", (void *)strtok_r}, {"fopen64", (void *)s_fopen}, {"fopen", (void *)s_fopen}, {"lseek64", (void *)lseek},
    {"access", (void *)s_access}, {"opendir", (void *)s_opendir},
    {"iconv", (void *)iconv},
    {"__ctype_b_loc", (void *)s_ctype_b_loc}, {"__ctype_tolower_loc", (void *)s_ctype_tolower_loc},
    {"__ctype_toupper_loc", (void *)s_ctype_toupper_loc},
    {"socket", (void *)s_net_refuse}, {"bind", (void *)s_net_refuse}, {"connect", (void *)s_net_refuse},
    {"listen", (void *)s_net_refuse}, {"accept", (void *)s_net_refuse}, {"getpeername", (void *)s_net_refuse},
    {"getsockname", (void *)s_net_refuse}, {"getsockopt", (void *)s_net_refuse}, {"setsockopt", (void *)s_net_refuse},
    {"recvfrom", (void *)s_net_refuse}, {"recvmsg", (void *)s_net_refuse}, {"sendmsg", (void *)s_net_refuse},
    {"sendto", (void *)s_net_refuse}, {"shutdown", (void *)s_net_refuse}, {"getnameinfo", (void *)s_net_refuse},
    {"getaddrinfo", (void *)s_getaddrinfo}, {"freeaddrinfo", (void *)s_freeaddrinfo},
    {"gai_strerror", (void *)s_gai_strerror}, {"gethostbyname", (void *)s_gethostbyname},
    {"shmget", (void *)s_shm_refuse}, {"shmdt", (void *)s_shm_refuse}, {"shmat", (void *)s_shmat},
    {"tcgetattr", (void *)s_tty_refuse}, {"tcsetattr", (void *)s_tty_refuse},
    {"mlock", (void *)s_mlock}, {"signal", (void *)s_signal}, {"fnmatch", (void *)s_fnmatch},
    {"__xpg_strerror_r", (void *)s_xpg_strerror_r}, {"__strdup", (void *)strdup},
    {"scandir", (void *)s_scandir}, {"alphasort", (void *)s_alphasort}, {"iconv_open", (void *)iconv_open}, {"iconv_close", (void *)iconv_close},
};

void *lnx_bind(const char *name) {
    for (size_t i = 0; i < sizeof g_lnx / sizeof g_lnx[0]; i++)
        if (!strcmp(g_lnx[i].name, name)) return g_lnx[i].addr;
    return NULL;
}

/* Linux clock ids -> macOS (syscall 228 = clock_gettime) */
long lnx_clock_gettime(long id, struct timespec *ts) {
    clockid_t m;
    switch (id) {
    case 0: case 5: m = CLOCK_REALTIME; break;
    case 1: case 6: case 7: m = CLOCK_MONOTONIC; break;
    case 4: m = CLOCK_MONOTONIC_RAW; break;
    case 2: m = CLOCK_PROCESS_CPUTIME_ID; break;
    case 3: m = CLOCK_THREAD_CPUTIME_ID; break;
    default: errno = EINVAL; return -1;
    }
    return clock_gettime(m, ts);
}
