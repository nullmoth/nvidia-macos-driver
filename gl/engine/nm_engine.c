/*
 * nm_engine.c: the engine bundle that takes the place of Apple's at
 * /System/Library/Frameworks/OpenGL.framework/Resources/GLEngine.bundle.
 *
 * Why it sits there: the window server opens that one path itself and resolves twelve gli entry points in it; the
 * framework additionally loads every bundle in the same folder whose name starts with "GLEngine". Apple's own
 * engine is kept beside this one under a name that does not start that way, and this bundle forwards to it.
 *
 * This first stage owns nothing: every call is forwarded unchanged, so with it installed the system behaves as it
 * did. What it can add is the record of the conversation: every entry call with its arguments, the parameter
 * number of every gliSetInteger / gliGetInteger with the bytes of its argument block, and the type and descriptor
 * of every drawable attach. The renderer of our own plugs in at the three places marked OWNED below; until it
 * exists those places forward too.
 *
 * This code is loaded into the window server, which runs as root, so nothing in a normal build is steered by the
 * environment: the engine it forwards to is the fixed path below and nothing else, and there is no file it can be
 * told to write. Three builds exist:
 *   default            forwards to the fixed path; records nothing.
 *   NM_ENGINE_SYSLOG   the same, and the record goes to the system log (syslog), never to a named file.
 *   NM_ENGINE_TEST     host tests only: NM_GL_APPLE_ENGINE names a stand-in engine and NM_GL_LOG a record file.
 *                      Both are ignored when the process is set-id or its effective user differs from its real
 *                      user, and the record file must be a regular file owned by the real user, opened without
 *                      following a link. A test build is never packaged.
 *
 * Failure rule: if Apple's engine cannot be opened, gliGetVersion answers 0. Both loaders treat that as "no
 * engine" and fall back to drawing without acceleration; nothing else here is reachable after that answer.
 */
#include "nm_gli.h"

#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#ifdef NM_ENGINE_TEST
#include <sys/stat.h>
#endif
#include <syslog.h>
#ifdef NM_ENGINE_OWNER
#include "../context/nm_own.h"
#endif

#define NM_EXPORT __attribute__((visibility("default")))

/* Where Apple's engine lives after installation. The folder name must not begin with "GLEngine": the framework
 * treats every such folder as a separate engine and would load Apple's a second time beside this one. */
#define NM_APPLE_ENGINE_PATH \
    "/System/Library/Frameworks/OpenGL.framework/Resources/NMOriginalEngine.bundle/GLEngine"

/*
 * Entry points whose signature is not needed here are forwarded with eight word-sized arguments: six cover the
 * integer argument registers of the 64-bit convention and two more cover the stack words gliInitializeLibrary
 * uses (seven arguments at the framework's call site). Reading a stack word the caller did not pass is harmless;
 * on i386 every argument is a stack word and the caller removes them.
 */
typedef intptr_t (*nm_fn8)(intptr_t, intptr_t, intptr_t, intptr_t, intptr_t, intptr_t, intptr_t, intptr_t);

#define NM_GLI_ENTRIES(X)                                                                                  \
    X(gliInitializeLibrary) X(gliTerminateLibrary) X(gliGetVersion) X(gliQueryRendererInfo)                \
    X(gliDestroyRendererInfo) X(gliChoosePixelFormat) X(gliDestroyPixelFormat) X(gliCreateContext)         \
    X(gliCreateContextWithShared) X(gliDestroyContext) X(gliAttachDrawable) X(gliAttachDrawableWithOptions)\
    X(gliSetInteger) X(gliGetInteger) X(gliGetAttribute) X(gliSetAttribute) X(gliCopyAttributes)           \
    X(gliGetMemoryAnnotationList) X(gliSwapBuffers)

enum {
#define X(n) NM_IDX_##n,
    NM_GLI_ENTRIES(X)
#undef X
    NM_IDX_COUNT
};
static const char *const nm_names[NM_IDX_COUNT] = {
#define X(n) #n,
    NM_GLI_ENTRIES(X)
#undef X
};

static struct {
    pthread_once_t once;
    void *handle;
    nm_fn8 fn[NM_IDX_COUNT];
    int log_fd;
} nm = { PTHREAD_ONCE_INIT, NULL, {0}, -1 };

/* True when this build records calls at all; the callers use it to skip building strings nobody reads. */
static inline int nm_recording(void)
{
#if defined(NM_ENGINE_SYSLOG)
    return 1;
#else
    return nm.log_fd >= 0;
#endif
}

/* One write() (or one syslog call) per line so lines from different threads never interleave.
 * Not static: the context layer records through it too (hidden visibility, never exported). */
void nm_log(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
void nm_log(const char *fmt, ...)
{
    if (!nm_recording()) return;
    char buf[1024];
    int n = snprintf(buf, sizeof buf, "%d ", (int)getpid());
    va_list ap;
    va_start(ap, fmt);
    int m = vsnprintf(buf + n, sizeof buf - (size_t)n - 1, fmt, ap);
    va_end(ap);
    if (m < 0) return;
    n += m;
    if (n > (int)sizeof buf - 2) n = (int)sizeof buf - 2;
#if defined(NM_ENGINE_SYSLOG)
    buf[n] = 0;
    syslog(LOG_NOTICE, "nm-gl %s", buf);   /* the text is passed as an argument, never as the format */
#else
    buf[n++] = '\n';
    ssize_t w = write(nm.log_fd, buf, (size_t)n);
    (void)w;
#endif
}

#ifdef NM_ENGINE_TEST
/* The two test variables are honoured only by an ordinary process: not set-id, effective user = real user. */
static int nm_test_env_allowed(void)
{
    return !issetugid() && geteuid() == getuid() && getegid() == getgid();
}

/* Opens the record file named by the test variable. The path is the caller's, so: never through a link, never
 * created for anyone but the owner, closed on exec, and accepted only if what was opened is a regular file that
 * the real user owns. Anything else is closed again and nothing is recorded. */
static int nm_test_open_log(const char *path)
{
    int flags = O_WRONLY | O_CREAT | O_APPEND | O_NOFOLLOW;
#ifdef O_CLOEXEC
    flags |= O_CLOEXEC;
#endif
    int fd = open(path, flags, 0600);
    if (fd < 0) return -1;
#ifndef O_CLOEXEC
    (void)fcntl(fd, F_SETFD, FD_CLOEXEC);
#endif
    struct stat st;
    if (fstat(fd, &st) != 0 || !S_ISREG(st.st_mode) || st.st_uid != getuid()) { close(fd); return -1; }
    return fd;
}
#endif

/* Hex of a small block, for the log. dst holds 2 * len + 1 bytes. */
static void nm_hex(char *dst, const void *src, size_t len)
{
    static const char d[] = "0123456789abcdef";
    const unsigned char *p = src;
    for (size_t i = 0; i < len; i++) { dst[2 * i] = d[p[i] >> 4]; dst[2 * i + 1] = d[p[i] & 15]; }
    dst[2 * len] = 0;
}

static void nm_open(void)
{
    const char *path = NM_APPLE_ENGINE_PATH;
#ifdef NM_ENGINE_TEST
    if (nm_test_env_allowed()) {
        const char *log = getenv("NM_GL_LOG");
        if (log && *log) nm.log_fd = nm_test_open_log(log);
        const char *alt = getenv("NM_GL_APPLE_ENGINE");
        if (alt && *alt) path = alt;
    }
#endif
    nm.handle = dlopen(path, RTLD_LAZY | RTLD_LOCAL);
    if (!nm.handle) {
        nm_log("open %s failed: %s", path, dlerror());
        return;
    }
    int found = 0;
    for (int i = 0; i < NM_IDX_COUNT; i++) {
        nm.fn[i] = (nm_fn8)dlsym(nm.handle, nm_names[i]);
        found += nm.fn[i] != NULL;
    }
    nm_log("open %s: %d of %d entry points present", path, found, NM_IDX_COUNT);
}

/*
 * A request this bundle does not serve itself goes to the original engine. Whether Apple's engine behaves the
 * same from a renamed folder is not yet known, so that hand-over is never silent: the first time each entry is
 * forwarded, one line goes to the system log, in every build (the text is fixed; nothing a caller controls).
 */
static unsigned char nm_forward_noted[NM_IDX_COUNT];
static inline nm_fn8 nm_apple(int idx)
{
    pthread_once(&nm.once, nm_open);
    if (nm.fn[idx] && !nm_forward_noted[idx]) {
        nm_forward_noted[idx] = 1;
        syslog(LOG_NOTICE, "nm-gl: %s is not served here; forwarding to the original engine", nm_names[idx]);
    }
    return nm.fn[idx];
}

#ifdef NM_ENGINE_OWNER
#define NM_OWNER 1
#else
/* Without the context layer nothing is owned; these make the routing below compile away. */
#define NM_OWNER 0
static inline int nm_own_active(void) { return 0; }
static inline int nm_own_is_context(const void *c) { (void)c; return 0; }
static inline long nm_own_set_integer(void *c, int p, const void *v) { (void)c; (void)p; (void)v; return 0; }
static inline long nm_own_get_integer(void *c, int p, void *v) { (void)c; (void)p; (void)v; return 0; }
static inline long nm_own_attach_drawable(void *c, int t, const void *d, int s, int o) { (void)c; (void)t; (void)d; (void)s; (void)o; return 0; }
#endif

/* ---- entry points with a measured signature ------------------------------------------------------------- */

/* Three pointers. Both loaders require a non-zero return with major 2 and minor 3, and an identifier that no
 * other engine reported; Apple's values are passed through, so this bundle is the one engine with that identifier. */
NM_EXPORT int gliGetVersion(int *major, int *minor, int *engine_id)
{
    nm_fn8 f = nm_apple(NM_IDX_gliGetVersion);
    if (!f) {
        /* no original engine: an engine in its own right only when the context layer serves requests */
        if (!nm_own_active()) return 0;
        if (major) *major = NM_GLI_VERSION_MAJOR;
        if (minor) *minor = NM_GLI_VERSION_MINOR;
        if (engine_id) *engine_id = 0x6e6d;
        return 1;
    }
    int r = (int)(f((intptr_t)major, (intptr_t)minor, (intptr_t)engine_id, 0, 0, 0, 0, 0) & 0xff);
    nm_log("gliGetVersion -> %d major=%d minor=%d id=0x%x", r, major ? *major : -1, minor ? *minor : -1,
           engine_id ? (unsigned)*engine_id : 0u);
    if (r && major && minor && (*major != NM_GLI_VERSION_MAJOR || *minor != NM_GLI_VERSION_MINOR))
        nm_log("gliGetVersion: engine reports %d.%d, the loaders expect %d.%d", *major, *minor,
               NM_GLI_VERSION_MAJOR, NM_GLI_VERSION_MINOR);
    return r;
}

static void nm_log_param(const char *who, intptr_t ctx, int pname, const void *value, intptr_t result, int after)
{
    if (!nm_recording()) return;
    size_t n = nm_cp_block_size(pname);
    char hex[2 * 64 + 1] = "";
    if (value && n && n <= 64) nm_hex(hex, value, n);
    nm_log("%s%s ctx=%p pname=%d block=%zu:%s -> %ld", who, after ? "" : " (before)", (void *)ctx, pname, n, hex,
           (long)result);
}

/* (context, parameter, pointer). OWNED later: 910, 997, 998, 999 on a context of ours. */
NM_EXPORT intptr_t gliSetInteger(intptr_t ctx, int pname, const void *value)
{
    if (nm_own_is_context((void *)ctx)) {
        intptr_t o = nm_own_set_integer((void *)ctx, pname, value);
        nm_log_param("gliSetInteger (own)", ctx, pname, value, o, 1);
        return o;
    }
    nm_fn8 f = nm_apple(NM_IDX_gliSetInteger);
    if (!f) return 10004; /* the framework's "invalid context" code: there is no engine to hold one */
    intptr_t r = f(ctx, pname, (intptr_t)value, 0, 0, 0, 0, 0);
    nm_log_param("gliSetInteger", ctx, pname, value, r, 1);
    return r;
}

/* (context, parameter, pointer). The window server's first call is (NULL, 224, &size). */
NM_EXPORT intptr_t gliGetInteger(intptr_t ctx, int pname, void *value)
{
    /* the table-size query comes with a null context; when the context layer serves requests the size is ours */
    if (nm_own_is_context((void *)ctx) || (nm_own_active() && !ctx && pname == NM_CP_DISPATCH_TABLE_SIZE)) {
        intptr_t o = nm_own_get_integer((void *)ctx, pname, value);
        nm_log_param("gliGetInteger (own)", ctx, pname, value, o, 1);
        return o;
    }
    nm_fn8 f = nm_apple(NM_IDX_gliGetInteger);
    if (!f) return 10004;
    intptr_t r = f(ctx, pname, (intptr_t)value, 0, 0, 0, 0, 0);
    nm_log_param("gliGetInteger", ctx, pname, value, r, 1);
    return r;
}

/* (context, type, descriptor, virtual screen, options). OWNED later: a context of ours attached to a surface. */
NM_EXPORT intptr_t gliAttachDrawableWithOptions(intptr_t ctx, int type, const void *desc, int screen, int options)
{
    if (nm_own_is_context((void *)ctx)) {
        intptr_t o = nm_own_attach_drawable((void *)ctx, type, desc, screen, options);
        nm_log("gliAttachDrawableWithOptions (own) ctx=%p type=0x%x -> %ld", (void *)ctx, type, (long)o);
        return o;
    }
    nm_fn8 f = nm_apple(NM_IDX_gliAttachDrawableWithOptions);
    if (!f) return 10004;
    if (nm_recording()) {
        size_t n = nm_drawable_desc_size(type);
        char hex[2 * 24 + 1] = "";
        if (desc && n) nm_hex(hex, desc, n);
        nm_log("gliAttachDrawableWithOptions ctx=%p type=0x%x desc=%zu:%s screen=%d options=0x%x", (void *)ctx,
               type, n, hex, screen, options);
    }
    intptr_t r = f(ctx, type, (intptr_t)desc, screen, options, 0, 0, 0);
    nm_log("gliAttachDrawableWithOptions -> %ld", (long)r);
    return r;
}

/* Apple's own gliAttachDrawable is the five-argument form with options 0. */
NM_EXPORT intptr_t gliAttachDrawable(intptr_t ctx, int type, const void *desc, int screen)
{
    return gliAttachDrawableWithOptions(ctx, type, desc, screen, 0);
}

/* ---- everything else: forwarded word for word ------------------------------------------------------------ */

#define NM_FORWARD(name)                                                                                   \
    NM_EXPORT intptr_t name(intptr_t a0, intptr_t a1, intptr_t a2, intptr_t a3, intptr_t a4, intptr_t a5,  \
                            intptr_t a6, intptr_t a7)                                                      \
    {                                                                                                      \
        nm_fn8 f = nm_apple(NM_IDX_##name);                                                                \
        if (!f) { nm_log(#name ": not available in the original engine"); return 0; }                      \
        intptr_t r = f(a0, a1, a2, a3, a4, a5, a6, a7);                                                    \
        nm_log(#name "(%p, %p, %p, %p, %p, %p, %p) -> %p", (void *)a0, (void *)a1, (void *)a2, (void *)a3, \
               (void *)a4, (void *)a5, (void *)a6, (void *)r);                                             \
        return r;                                                                                          \
    }

NM_FORWARD(gliTerminateLibrary)
NM_FORWARD(gliCreateContextWithShared)
NM_FORWARD(gliGetAttribute)
NM_FORWARD(gliSetAttribute)
NM_FORWARD(gliCopyAttributes)
NM_FORWARD(gliGetMemoryAnnotationList) /* 10.9 and later; absent from the 10.7 and 10.8 engines */
NM_FORWARD(gliSwapBuffers)             /* resolved by the 10.7 window server only */

/* ---- entries the context layer can serve ---------------------------------------------------------------- */
#if NM_OWNER

/* Seven arguments at the framework's call site; the first three are the display services, their framebuffer
 * indices and the count. The context layer looks at them first, then the original engine is initialised too. */
NM_EXPORT intptr_t gliInitializeLibrary(intptr_t a0, intptr_t a1, intptr_t a2, intptr_t a3, intptr_t a4, intptr_t a5,
                                        intptr_t a6, intptr_t a7)
{
    nm_own_library_init((const uint32_t *)a0, (const uint8_t *)a1, (unsigned)a2);
    nm_fn8 f = nm_apple(NM_IDX_gliInitializeLibrary);
    intptr_t r = f ? f(a0, a1, a2, a3, a4, a5, a6, a7) : 0;
    nm_log("gliInitializeLibrary(count %u) -> %p%s", (unsigned)a2, (void *)r, f ? "" : " (no original engine)");
    return r;
}

/* The engine's objects are linked lists of nodes. A node of ours goes first and points at the original engine's
 * list, so a caller that walks the list sees both and a caller that takes the first node gets ours. */
NM_EXPORT intptr_t gliQueryRendererInfo(void **out, unsigned display_mask)
{
    if (!out) return 10008;
    *out = NULL;
    nm_fn8 f = nm_apple(NM_IDX_gliQueryRendererInfo);
    intptr_t r = f ? f((intptr_t)out, display_mask, 0, 0, 0, 0, 0, 0) : 10003;
    void **mine = nm_own_renderer_info(display_mask);
    if (mine) { *mine = (r == 0) ? *out : NULL; *out = mine; r = 0; }
    nm_log("gliQueryRendererInfo(mask 0x%x) -> %ld, first node %s", display_mask, (long)r, mine ? "ours" : "the original engine's");
    return r;
}

NM_EXPORT intptr_t gliChoosePixelFormat(void **out, const void *attributes)
{
    if (!out) return 10008;
    *out = NULL;
    nm_fn8 f = nm_apple(NM_IDX_gliChoosePixelFormat);
    intptr_t r = f ? f((intptr_t)out, (intptr_t)attributes, 0, 0, 0, 0, 0, 0) : 10002;
    void **mine = nm_own_pixel_format(attributes);
    if (mine) { *mine = (r == 0) ? *out : NULL; *out = mine; r = 0; }
    nm_log("gliChoosePixelFormat -> %ld, first node %s", (long)r, mine ? "ours" : "the original engine's");
    return r;
}

static intptr_t nm_destroy_list(void *node, int idx, int (*is_ours)(const void *))
{
    void *rest = node;
    if (is_ours(node)) { rest = *(void **)node; nm_own_free_node(node); }
    if (!rest) return 0;
    nm_fn8 f = nm_apple(idx);
    return f ? f((intptr_t)rest, 0, 0, 0, 0, 0, 0, 0) : 0;
}
NM_EXPORT intptr_t gliDestroyRendererInfo(void *node) { return nm_destroy_list(node, NM_IDX_gliDestroyRendererInfo, nm_own_is_renderer_info); }
NM_EXPORT intptr_t gliDestroyPixelFormat(void *node)  { return nm_destroy_list(node, NM_IDX_gliDestroyPixelFormat, nm_own_is_pixel_format); }

/* (out context, pixel format, share, table, second table, flags), as the 10.11.6 window server calls it. A pixel
 * format of ours makes a context of ours; any other goes to the engine that made it. */
NM_EXPORT intptr_t gliCreateContext(void **rend_out, const void *pixel_format, void *share, void *table, void *dummy_table, intptr_t flags)
{
    if (nm_own_is_pixel_format(pixel_format)) {
        intptr_t o = nm_own_create_context(rend_out, pixel_format, share, table, dummy_table, flags);
        nm_log("gliCreateContext (own) -> %ld", (long)o);
        return o;
    }
    nm_fn8 f = nm_apple(NM_IDX_gliCreateContext);
    if (!f) return 10002;
    return f((intptr_t)rend_out, (intptr_t)pixel_format, (intptr_t)share, (intptr_t)table, (intptr_t)dummy_table, flags, 0, 0);
}

NM_EXPORT intptr_t gliDestroyContext(void *rend)
{
    if (nm_own_is_context(rend)) return nm_own_destroy_context(rend);
    nm_fn8 f = nm_apple(NM_IDX_gliDestroyContext);
    return f ? f((intptr_t)rend, 0, 0, 0, 0, 0, 0, 0) : 10004;
}

#else
NM_FORWARD(gliInitializeLibrary)       /* seven arguments at the framework's call site */
NM_FORWARD(gliQueryRendererInfo)
NM_FORWARD(gliDestroyRendererInfo)
NM_FORWARD(gliChoosePixelFormat)
NM_FORWARD(gliDestroyPixelFormat)
NM_FORWARD(gliCreateContext)
NM_FORWARD(gliDestroyContext)
#endif
