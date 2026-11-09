/*
 * elfhost.c - load an x86-64 Linux ELF shared object into a macOS x86-64 process.
 *
 * Written from the ELF-64 and System V x86-64 psABI specifications. Both platforms share the SysV calling
 * convention, so code in the object runs unchanged; only its imports differ. Every import is bound to a
 * macOS function: either the identically named libc function, or a shim below that translates a Linux
 * ABI difference (flag values, struct sizes, TLS, the dynamic-linker interface).
 *
 * Fail loud: an import with no binding is reported at load time and bound to a trap that names it, so a
 * missing shim can never become a silent wrong answer.
 */
#include "elfhost.h"
#include "lnxlibc.h"

#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <locale.h>
#include <langinfo.h>
#include <mach-o/dyld.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>
#include <wchar.h>
#include <wctype.h>
#include <xlocale.h>

/* ---- ELF-64 structures (ELF spec, sections 1-2) ------------------------------------------------ */
typedef struct { unsigned char ident[16]; uint16_t type, machine; uint32_t version; uint64_t entry, phoff, shoff;
                 uint32_t flags; uint16_t ehsize, phentsize, phnum, shentsize, shnum, shstrndx; } Ehdr;
typedef struct { uint32_t type, flags; uint64_t offset, vaddr, paddr, filesz, memsz, align; } Phdr;
typedef struct { uint32_t name, type; uint64_t flags, addr, offset, size; uint32_t link, info;
                 uint64_t addralign, entsize; } Shdr;
typedef struct { uint32_t name; unsigned char info, other; uint16_t shndx; uint64_t value, size; } Sym;
typedef struct { uint64_t offset, info; int64_t addend; } Rela;
typedef struct { int64_t tag; uint64_t val; } Dyn;

enum { PT_LOAD = 1, PT_DYNAMIC = 2, PT_TLS = 7, PF_X = 1, PF_W = 2, PF_R = 4, SHT_DYNSYM = 11 };
enum { DT_NULL = 0, DT_STRTAB = 5, DT_SYMTAB = 6, DT_RELA = 7, DT_RELASZ = 8, DT_INIT = 12, DT_FINI = 13,
       DT_JMPREL = 23, DT_PLTRELSZ = 2, DT_INIT_ARRAY = 25, DT_INIT_ARRAYSZ = 27 };
enum { R_64 = 1, R_GLOB_DAT = 6, R_JUMP_SLOT = 7, R_RELATIVE = 8, R_DTPMOD64 = 16, R_DTPOFF64 = 17 };

struct elfhost_module {
    char path[1024]; const char *base_name;
    uint8_t *base; size_t span;
    const Phdr *phdr; uint16_t phnum;
    const Sym *dynsym; size_t nsym; const char *dynstr;
    /* PT_TLS template; id is this module's dynamic TLS module id (1-based, = load order) */
    const uint8_t *tls_init; size_t tls_filesz, tls_memsz, tls_align; size_t id; pthread_key_t tls_key;
    int unresolved;
};

/* Every loaded module, in load order. dlopen/dlsym/dladdr/dl_iterate_phdr and TLS all work across them,
 * as the dynamic linker would for a process with several shared objects. */
#define MAXMOD 32
static elfhost_module *g_mods[MAXMOD]; static size_t g_nmod;
static pthread_mutex_t g_load_lock = PTHREAD_MUTEX_INITIALIZER;   /* dlopen can nest inside a constructor: recursive */
static pthread_once_t g_load_lock_once = PTHREAD_ONCE_INIT;
static void load_lock_init(void) {
    pthread_mutexattr_t a; pthread_mutexattr_init(&a); pthread_mutexattr_settype(&a, PTHREAD_MUTEX_RECURSIVE);
    pthread_mutex_init(&g_load_lock, &a); pthread_mutexattr_destroy(&a);
}
static int g_trace = 0;

#define LOG(...) do { if (g_trace) fprintf(stderr, "[elfhost] " __VA_ARGS__); } while (0)

/* ---- TLS: general-dynamic model through __tls_get_addr (psABI 3.4.x), one block per module per thread -- */
typedef struct { uint64_t module, offset; } tls_index;
static void *sh___tls_get_addr(tls_index *ti) {
    if (!ti->module || ti->module > g_nmod) { fprintf(stderr, "[elfhost] FATAL: TLS module %llu\n", (unsigned long long)ti->module); abort(); }
    elfhost_module *m = g_mods[ti->module - 1];
    uint8_t *blk = pthread_getspecific(m->tls_key);
    if (!blk) {
        size_t a = m->tls_align ? m->tls_align : 16;
        if (posix_memalign((void **)&blk, a < 16 ? 16 : a, m->tls_memsz ? m->tls_memsz : 16)) abort();
        memset(blk, 0, m->tls_memsz);
        memcpy(blk, m->tls_init, m->tls_filesz);
        pthread_setspecific(m->tls_key, blk);
    }
    return blk + ti->offset;
}

/* ---- the dynamic-linker interface ------------------------------------------------------------- */
#define LNX_RTLD_DEFAULT ((void *)0)
static void *const MAIN_HANDLE = (void *)0x4d41494eULL;   /* "MAIN": dlopen(NULL) */
static const elfhost_resolver *g_extra; static void *g_extra_ctx;
static char g_dlerr[512]; static int g_dlerr_set;

static void *resolve_name(const char *name);
static elfhost_module *find_loaded(const char *file) {
    const char *b = strrchr(file, '/'); b = b ? b + 1 : file;
    for (size_t i = 0; i < g_nmod; i++) if (!strcmp(g_mods[i]->base_name, b) || !strcmp(g_mods[i]->path, file)) return g_mods[i];
    return NULL;
}
static int is_module(const void *h) { for (size_t i = 0; i < g_nmod; i++) if (g_mods[i] == h) return 1; return 0; }
static void *module_export(elfhost_module *m, const char *name) {
    for (size_t i = 1; i < m->nsym; i++) {
        const Sym *s = &m->dynsym[i];
        if (s->shndx && (s->info >> 4) != 0 && !strcmp(m->dynstr + s->name, name)) return m->base + s->value;
    }
    return NULL;
}

static void *sh_dlopen(const char *file, int flags) {
    LOG("dlopen(%s, 0x%x)\n", file ? file : "NULL", flags);
    if (!file) return MAIN_HANDLE;
    /* NVIDIA's NGX core verifies a snippet's signature on an open fd, then loads "/proc/self/fd/N" so the file it
     * checked is the file that runs. macOS has no /proc: resolve the fd to its path and load that. */
    char fdpath[1024]; int fdn;
    if (sscanf(file, "/proc/self/fd/%d", &fdn) == 1) {
        if (fcntl(fdn, F_GETPATH, fdpath) == -1) { snprintf(g_dlerr, sizeof g_dlerr, "%s: bad fd", file); g_dlerr_set = 1; return NULL; }
        LOG("dlopen %s -> %s\n", file, fdpath);
        /* the path may already be unlinked (the core loads a verified temp copy): open through the fd itself */
        char devfd[32]; snprintf(devfd, sizeof devfd, "/dev/fd/%d", fdn);
        char err[256]; elfhost_module *fm = elfhost_load(devfd, NULL, NULL, err, sizeof err);
        if (!fm) { snprintf(g_dlerr, sizeof g_dlerr, "%s: %s", fdpath, err); g_dlerr_set = 1; LOG("dlopen fd %d: %s\n", fdn, err); return NULL; }
        snprintf(fm->path, sizeof fm->path, "%s", fdpath);
        { const char *b = strrchr(fm->path, '/'); fm->base_name = b ? b + 1 : fm->path; }
        return fm;
    }
    elfhost_module *m = find_loaded(file);
    if (m) return m;
    if (g_extra && g_extra->dlopen_hook) {
        void *h = g_extra->dlopen_hook(g_extra_ctx, file, flags);   /* a stand-in library, or a path to load */
        if (h) return h;
    }
    /* search ELFHOST_PATH (colon-separated directories) for the file by base name */
    const char *sp = getenv("ELFHOST_PATH"); const char *b = strrchr(file, '/'); b = b ? b + 1 : file;
    if (file[0] == '/' && access(file, R_OK) == 0) {
        char err[256]; m = elfhost_load(file, NULL, NULL, err, sizeof err);
        if (m) return m;
    }
    for (const char *p = sp; p && *p;) {
        const char *c = strchr(p, ':'); size_t n = c ? (size_t)(c - p) : strlen(p);
        char path[1024]; snprintf(path, sizeof path, "%.*s/%s", (int)n, p, b);
        if (access(path, R_OK) == 0) {
            char err[256]; m = elfhost_load(path, NULL, NULL, err, sizeof err);
            if (m) return m;
            LOG("dlopen %s: %s\n", path, err);
        }
        p = c ? c + 1 : NULL;
    }
    snprintf(g_dlerr, sizeof g_dlerr, "%s: cannot open shared object file", file); g_dlerr_set = 1;
    return NULL;
}
static void *sh_dlsym(void *h, const char *name) {
    LOG("dlsym(%p, %s)\n", h, name);
    if (h == MAIN_HANDLE || h == LNX_RTLD_DEFAULT) return resolve_name(name);
    if (is_module(h)) return module_export(h, name);
    if (g_extra && g_extra->dlsym_hook) return g_extra->dlsym_hook(g_extra_ctx, h, name);
    return NULL;
}
static int sh_dlclose(void *h) { (void)h; return 0; }   /* modules stay mapped for the life of the process */
static char *sh_dlerror(void) { if (!g_dlerr_set) return NULL; g_dlerr_set = 0; return g_dlerr; }

typedef struct { const char *fname; void *fbase; const char *sname; void *saddr; } lnx_Dl_info;   /* same as macOS */
static int sh_dladdr(const void *addr, lnx_Dl_info *info) {
    for (size_t i = 0; i < g_nmod; i++) {
        elfhost_module *m = g_mods[i];
        if ((const uint8_t *)addr >= m->base && (const uint8_t *)addr < m->base + m->span) {
            info->fname = m->path; info->fbase = m->base; info->sname = NULL; info->saddr = NULL;
            return 1;
        }
    }
    return dladdr(addr, (Dl_info *)info);
}

/* Linux struct dl_phdr_info; libgcc's unwinder walks it to find .eh_frame_hdr for C++ exceptions. */
struct lnx_dl_phdr_info { uint64_t addr; const char *name; const Phdr *phdr; uint16_t phnum;
                          unsigned long long adds, subs; size_t tls_modid; void *tls_data; };
static int sh_dl_iterate_phdr(int (*cb)(struct lnx_dl_phdr_info *, size_t, void *), void *data) {
    int r = 0;
    for (size_t i = 0; i < g_nmod && !r; i++) {
        elfhost_module *m = g_mods[i];
        struct lnx_dl_phdr_info inf = { (uint64_t)m->base, m->path, m->phdr, m->phnum, g_nmod, 0,
                                        m->tls_memsz ? m->id : 0, NULL };
        r = cb(&inf, sizeof inf, data);
    }
    return r;
}

static ssize_t sh_readlink(const char *p, char *buf, size_t n) {
    LOG("readlink(%s)\n", p);
    int fdn;
    if (sscanf(p, "/proc/self/fd/%d", &fdn) == 1) {
        char tmp[1024]; if (fcntl(fdn, F_GETPATH, tmp) == -1) { errno = EBADF; return -1; }
        size_t l = strlen(tmp); if (l > n) l = n; memcpy(buf, tmp, l); return (ssize_t)l;
    }
    if (!strcmp(p, "/proc/self/exe")) {
        char tmp[4096]; uint32_t sz = sizeof tmp;
        if (_NSGetExecutablePath(tmp, &sz)) { errno = ENAMETOOLONG; return -1; }
        size_t l = strlen(tmp); if (l > n) l = n; memcpy(buf, tmp, l); return (ssize_t)l;   /* no NUL, as Linux */
    }
    return readlink(p, buf, n);
}

/* ---- flag-value translations (Linux asm-generic/fcntl.h vs macOS sys/fcntl.h) ------------------- */
static int open_flags(int l) {
    int m = l & 3;   /* O_RDONLY/WRONLY/RDWR agree */
    static const int map[][2] = { {0x40, O_CREAT}, {0x80, O_EXCL}, {0x200, O_TRUNC}, {0x400, O_APPEND},
                                  {0x800, O_NONBLOCK}, {0x80000, O_CLOEXEC}, {0x10000, O_DIRECTORY},
                                  {0x20000, O_NOFOLLOW}, {0x100, O_NOCTTY} };
    for (size_t i = 0; i < sizeof map / sizeof map[0]; i++) if (l & map[i][0]) m |= map[i][1];
    return m;
}
static int sh_open(const char *p, int fl, ...) {
    va_list ap; va_start(ap, fl); int mode = (fl & 0x40) ? va_arg(ap, int) : 0; va_end(ap);
    LOG("open(%s, 0x%x)\n", p, fl);
    return open(p, open_flags(fl), mode);
}
static int sh_fcntl(int fd, int cmd, ...) {
    va_list ap; va_start(ap, cmd); long arg = va_arg(ap, long); va_end(ap);
    switch (cmd) {
    case 1: case 2: return fcntl(fd, cmd, (int)arg);              /* F_GETFD/F_SETFD: FD_CLOEXEC = 1 on both */
    case 3: return fcntl(fd, F_GETFL);
    case 4: return fcntl(fd, F_SETFL, open_flags((int)arg));
    default: LOG("fcntl cmd %d unsupported\n", cmd); errno = EINVAL; return -1;
    }
}

static long sh_syscall(long nr, ...) {
    if (nr == 186) { uint64_t t; pthread_threadid_np(NULL, &t); return (long)t; }   /* gettid */
    if (nr == 228) {                                                                 /* clock_gettime */
        va_list ap; va_start(ap, nr); long id = va_arg(ap, long); struct timespec *ts = va_arg(ap, struct timespec *); va_end(ap);
        return lnx_clock_gettime(id, ts);
    }
    LOG("syscall(%ld) -> ENOSYS\n", nr);
    errno = ENOSYS; return -1;
}

/* pthread_once on Linux is a 4-byte int: 0 = not run. 1 = running, 2 = done. */
static int sh_pthread_once(_Atomic int *o, void (*fn)(void)) {
    int z = 0;
    if (atomic_load(o) == 2) return 0;
    if (atomic_compare_exchange_strong(o, &z, 1)) { fn(); atomic_store(o, 2); return 0; }
    while (atomic_load(o) != 2) sched_yield();
    return 0;
}
/* ---- glibc locale entry points -> macOS xlocale -------------------------------------------------- */
static locale_t sh_newlocale(int mask, const char *name, locale_t base) {
    (void)mask;   /* glibc masks differ; libstdc++ only ever asks for all categories here */
    return newlocale(LC_ALL_MASK, name, base);
}
static char *sh_nl_langinfo_l(int item, locale_t l) {
    /* glibc item numbers differ from macOS; libstdc++ reads CODESET and a few names. Map the codeset,
     * answer the rest from the "C" locale, which is what this object is initialised with. */
    if (item == 14) return nl_langinfo_l(CODESET, l);          /* glibc CODESET = 14 */
    return "";
}
static size_t sh_mb_cur_max(void) { return MB_CUR_MAX; }
static char *sh_gettext(const char *m) { return (char *)m; }
static char *sh_dgettext(const char *d, const char *m) { (void)d; return (char *)m; }
static char *sh_bind_codeset(const char *d, const char *c) { (void)d; return (char *)c; }
static void sh_noop(void) {}

/* ---- the binding table ---------------------------------------------------------------------- */
typedef struct { const char *name; void *addr; } binding;
static const binding g_bind[] = {
    {"__errno_location", (void *)__error},
    {"stdin", (void *)&__stdinp}, {"stdout", (void *)&__stdoutp}, {"stderr", (void *)&__stderrp},
    {"__tls_get_addr", (void *)sh___tls_get_addr},
    {"dlopen", (void *)sh_dlopen}, {"dlsym", (void *)sh_dlsym}, {"dlclose", (void *)sh_dlclose},
    {"dladdr", (void *)sh_dladdr}, {"dlerror", (void *)sh_dlerror}, {"dl_iterate_phdr", (void *)sh_dl_iterate_phdr},
    {"readlink", (void *)sh_readlink}, {"open", (void *)sh_open}, {"fcntl", (void *)sh_fcntl},
    {"syscall", (void *)sh_syscall},
    {"fseeko64", (void *)fseeko}, {"ftello64", (void *)ftello},
    {"pthread_once", (void *)sh_pthread_once},
    {"__newlocale", (void *)sh_newlocale}, {"newlocale", (void *)sh_newlocale},
    {"__uselocale", (void *)uselocale}, {"__freelocale", (void *)freelocale}, {"__duplocale", (void *)duplocale},
    {"__nl_langinfo_l", (void *)sh_nl_langinfo_l}, {"__ctype_get_mb_cur_max", (void *)sh_mb_cur_max},
    {"__iswctype_l", (void *)iswctype_l}, {"__wctype_l", (void *)wctype_l},
    {"__towlower_l", (void *)towlower_l}, {"__towupper_l", (void *)towupper_l},
    {"__strcoll_l", (void *)strcoll_l}, {"__strxfrm_l", (void *)strxfrm_l},
    {"__wcscoll_l", (void *)wcscoll_l}, {"__wcsxfrm_l", (void *)wcsxfrm_l},
    {"__strftime_l", (void *)strftime_l}, {"__wcsftime_l", (void *)wcsftime_l},
    {"__strtod_l", (void *)strtod_l}, {"__strtof_l", (void *)strtof_l}, {"strtold_l", (void *)strtold_l},
    {"gettext", (void *)sh_gettext}, {"dgettext", (void *)sh_dgettext},
    {"bind_textdomain_codeset", (void *)sh_bind_codeset},
    {"__cxa_finalize", (void *)sh_noop},
    {"quick_exit", (void *)_Exit},
};
/* Weak, optional hooks a Linux object tolerates as NULL. */
static const char *const g_weak_null[] = { "__gmon_start__", "_ITM_deregisterTMCloneTable", "_ITM_registerTMCloneTable" };

static void *resolve_name(const char *name) {
    { void *l = lnx_bind(name); if (l) return l; }
    for (size_t i = 0; i < sizeof g_bind / sizeof g_bind[0]; i++)
        if (!strcmp(g_bind[i].name, name)) return g_bind[i].addr;
    if (g_extra && g_extra->resolve) { void *p = g_extra->resolve(g_extra_ctx, name); if (p) return p; }
    for (size_t i = 0; i < g_nmod; i++) { void *p = module_export(g_mods[i], name); if (p) return p; }   /* global scope */
    return dlsym(RTLD_DEFAULT, name);   /* same name, same SysV ABI: malloc, memcpy, fopen, printf, ... */
}

/* Unresolved imports land here. The PLT/GOT slot can't tell us which one, so each gets a numbered trap. */
#define NTRAP 32
static const char *g_trap_name[NTRAP];
#define TRAP(n) static void trap##n(void) { fprintf(stderr, "[elfhost] FATAL: unbound import %s called\n", g_trap_name[n]); abort(); }
TRAP(0) TRAP(1) TRAP(2) TRAP(3) TRAP(4) TRAP(5) TRAP(6) TRAP(7) TRAP(8) TRAP(9) TRAP(10) TRAP(11) TRAP(12) TRAP(13)
TRAP(14) TRAP(15) TRAP(16) TRAP(17) TRAP(18) TRAP(19) TRAP(20) TRAP(21) TRAP(22) TRAP(23) TRAP(24) TRAP(25) TRAP(26)
TRAP(27) TRAP(28) TRAP(29) TRAP(30) TRAP(31)
static void (*const g_traps[NTRAP])(void) = { trap0, trap1, trap2, trap3, trap4, trap5, trap6, trap7, trap8, trap9,
    trap10, trap11, trap12, trap13, trap14, trap15, trap16, trap17, trap18, trap19, trap20, trap21, trap22, trap23,
    trap24, trap25, trap26, trap27, trap28, trap29, trap30, trap31 };

static void *bind_import(elfhost_module *m, const char *name) {
    for (size_t i = 0; i < sizeof g_weak_null / sizeof g_weak_null[0]; i++)
        if (!strcmp(g_weak_null[i], name)) return NULL;
    void *p = resolve_name(name);
    if (p) return p;
    fprintf(stderr, "[elfhost] UNRESOLVED import: %s\n", name);
    int n = m->unresolved++;
    if (n < NTRAP) { g_trap_name[n] = name; return (void *)g_traps[n]; }
    return (void *)abort;
}

/* ---- loading ------------------------------------------------------------------------------------ */
static int fail(char *err, size_t n, const char *msg) { snprintf(err, n, "%s", msg); return -1; }

elfhost_module *elfhost_load(const char *path, const elfhost_resolver *extra, void *ctx, char *err, size_t errn) {
    if (extra) { g_extra = extra; g_extra_ctx = ctx; }   /* nested loads (dlopen from a constructor) keep the first hooks */
    g_trace = getenv("ELFHOST_TRACE") != NULL;
    pthread_once(&g_load_lock_once, load_lock_init);
    if (g_nmod >= MAXMOD) { fail(err, errn, "too many modules"); return NULL; }
    int fd = open(path, O_RDONLY);
    if (fd < 0) { fail(err, errn, "open failed"); return NULL; }
    struct stat st; fstat(fd, &st);
    uint8_t *file = mmap(NULL, st.st_size, PROT_READ, MAP_PRIVATE, fd, 0); close(fd);
    if (file == MAP_FAILED) { fail(err, errn, "mmap of file failed"); return NULL; }
    const Ehdr *eh = (const Ehdr *)file;
    if ((size_t)st.st_size < sizeof *eh || memcmp(eh->ident, "\x7f" "ELF\x02\x01", 6) || eh->machine != 62 ||
        eh->type != 3 || eh->phoff + (uint64_t)eh->phnum * sizeof(Phdr) > (uint64_t)st.st_size) {
        fail(err, errn, "not an x86-64 ELF shared object"); munmap(file, st.st_size); return NULL;
    }
    const Phdr *ph = (const Phdr *)(file + eh->phoff);
    uint64_t lo = UINT64_MAX, hi = 0;
    for (int i = 0; i < eh->phnum; i++) if (ph[i].type == PT_LOAD) {
        if (ph[i].offset + ph[i].filesz > (uint64_t)st.st_size) { fail(err, errn, "segment past end of file"); munmap(file, st.st_size); return NULL; }
        if (ph[i].vaddr < lo) lo = ph[i].vaddr;
        if (ph[i].vaddr + ph[i].memsz > hi) hi = ph[i].vaddr + ph[i].memsz;
    }
    if (lo != 0 || hi == 0) { fail(err, errn, "unexpected PT_LOAD layout"); munmap(file, st.st_size); return NULL; }

    elfhost_module *m = calloc(1, sizeof *m);
    snprintf(m->path, sizeof m->path, "%s", path);
    { const char *b = strrchr(m->path, '/'); m->base_name = b ? b + 1 : m->path; }
    m->span = (hi + 0xfff) & ~0xfffULL;
    m->base = mmap(NULL, m->span, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
    if (m->base == MAP_FAILED) { free(m); munmap(file, st.st_size); fail(err, errn, "mmap image failed"); return NULL; }
    for (int i = 0; i < eh->phnum; i++) if (ph[i].type == PT_LOAD)
        memcpy(m->base + ph[i].vaddr, file + ph[i].offset, ph[i].filesz);   /* bss stays zero from MAP_ANON */
    m->phdr = (const Phdr *)(m->base + eh->phoff); m->phnum = eh->phnum;   /* first PT_LOAD covers the headers */

    const Dyn *dyn = NULL;
    for (int i = 0; i < eh->phnum; i++) {
        if (ph[i].type == PT_DYNAMIC) dyn = (const Dyn *)(m->base + ph[i].vaddr);
        if (ph[i].type == PT_TLS) { m->tls_init = m->base + ph[i].vaddr; m->tls_filesz = ph[i].filesz;
                                    m->tls_memsz = ph[i].memsz; m->tls_align = ph[i].align; }
    }
    /* dynsym size comes from its section header (the ELF spec gives no count in DT_*) */
    const Shdr *sh = (const Shdr *)(file + eh->shoff);
    for (int i = 0; eh->shoff && i < eh->shnum; i++)
        if (sh[i].type == SHT_DYNSYM) m->nsym = sh[i].size / sizeof(Sym);
    uint64_t rela = 0, relasz = 0, jmprel = 0, pltsz = 0, init = 0, ia = 0, iasz = 0;
    for (const Dyn *d = dyn; d && d->tag != DT_NULL; d++) switch (d->tag) {
        case DT_STRTAB: m->dynstr = (const char *)(m->base + d->val); break;
        case DT_SYMTAB: m->dynsym = (const Sym *)(m->base + d->val); break;
        case DT_RELA: rela = d->val; break;      case DT_RELASZ: relasz = d->val; break;
        case DT_JMPREL: jmprel = d->val; break;  case DT_PLTRELSZ: pltsz = d->val; break;
        case DT_INIT: init = d->val; break;
        case DT_INIT_ARRAY: ia = d->val; break;  case DT_INIT_ARRAYSZ: iasz = d->val; break;
    }
    munmap(file, st.st_size);
    if (!m->dynsym || !m->dynstr || !m->nsym) { fail(err, errn, "no dynamic symbol table"); return NULL; }
    /* register before relocating: the object's own TLS id and global-scope lookups must see it */
    pthread_mutex_lock(&g_load_lock);
    g_mods[g_nmod] = m; m->id = ++g_nmod; pthread_key_create(&m->tls_key, free);
    pthread_mutex_unlock(&g_load_lock);

    int counts[32] = {0};
    for (int pass = 0; pass < 2; pass++) {
        const Rela *r = (const Rela *)(m->base + (pass ? jmprel : rela));
        size_t n = (pass ? pltsz : relasz) / sizeof(Rela);
        for (size_t i = 0; i < n; i++) {
            uint64_t *P = (uint64_t *)(m->base + r[i].offset);
            uint32_t type = (uint32_t)r[i].info, si = (uint32_t)(r[i].info >> 32);
            const Sym *s = &m->dynsym[si];
            const char *name = m->dynstr + s->name;
            void *S = NULL;
            if ((type == R_64 || type == R_GLOB_DAT || type == R_JUMP_SLOT) && si)
                S = s->shndx ? (void *)(m->base + s->value) : bind_import(m, name);
            switch (type) {
            case R_RELATIVE: *P = (uint64_t)m->base + r[i].addend; break;
            case R_64: *P = (uint64_t)S + r[i].addend; break;
            case R_GLOB_DAT: case R_JUMP_SLOT: *P = (uint64_t)S; break;
            case R_DTPMOD64: *P = m->id; break;
            case R_DTPOFF64: *P = s->value + r[i].addend; break;
            default: { char b[64]; snprintf(b, sizeof b, "unsupported relocation type %u", type); fail(err, errn, b); return NULL; }
            }
            counts[type & 31]++;
        }
    }
    LOG("relocations: RELATIVE %d, 64 %d, GLOB_DAT %d, JUMP_SLOT %d, DTPMOD %d, DTPOFF %d; unresolved %d\n",
        counts[R_RELATIVE], counts[R_64], counts[R_GLOB_DAT], counts[R_JUMP_SLOT], counts[R_DTPMOD64],
        counts[R_DTPOFF64], m->unresolved);

    /* final protections per segment (W^X); pages shared by two segments keep the union */
    for (int i = 0; i < m->phnum; i++) if (m->phdr[i].type == PT_LOAD) {
        uint64_t a = m->phdr[i].vaddr & ~0xfffULL, e = (m->phdr[i].vaddr + m->phdr[i].memsz + 0xfff) & ~0xfffULL;
        int prot = ((m->phdr[i].flags & PF_R) ? PROT_READ : 0) | ((m->phdr[i].flags & PF_W) ? PROT_WRITE : 0) |
                   ((m->phdr[i].flags & PF_X) ? PROT_EXEC : 0);
        if (mprotect(m->base + a, e - a, prot)) { fail(err, errn, "mprotect failed"); return NULL; }
    }

    /* constructors: DT_INIT, then DT_INIT_ARRAY in order (ELF spec, "Initialization and Termination") */
    static char *argv0[] = { "elfhost", NULL }; extern char **environ;
    if (init) ((void (*)(int, char **, char **))(m->base + init))(1, argv0, environ);
    for (uint64_t i = 0; i < iasz / 8; i++) {
        uint64_t fn = ((uint64_t *)(m->base + ia))[i];
        if (fn && fn != (uint64_t)-1) ((void (*)(int, char **, char **))fn)(1, argv0, environ);
    }
    return m;
}

void *elfhost_sym(elfhost_module *m, const char *name) {
    for (size_t i = 1; i < m->nsym; i++) {
        const Sym *s = &m->dynsym[i];
        if (s->shndx && (s->info & 0xf) == 2 && !strcmp(m->dynstr + s->name, name)) return m->base + s->value;
    }
    return NULL;
}
int elfhost_unresolved(const elfhost_module *m) { return m->unresolved; }
