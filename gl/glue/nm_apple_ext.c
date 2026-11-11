/*
 * nm_apple_ext.c: the Apple-extension table entries that Mesa does not have and that Apple's own frameworks call.
 * The window server calls flush-render, the fence and object tests, the purgeable pair, texture range and (10.10,
 * 10.11) the responsible-process label; the rest of each extension is here so no half of a pair is refused.
 *
 * Fences are built on Mesa's sync objects. The fence name table is per process, not per share group: fence names
 * are small integers handed out here, and no caller read so far uses the same fence from two share groups.
 */
#include <OpenGL/gliDispatch.h>
#include "nm_glue.h"

#include <pthread.h>
#include <stddef.h>

#define GL_FENCE_APPLE                    0x8A0B
#define GL_RETAINED_APPLE                 0x8A1B
#define GL_SYNC_GPU_COMMANDS_COMPLETE     0x9117
#define GL_ALREADY_SIGNALED               0x911A
#define GL_CONDITION_SATISFIED            0x911C
#define GL_SYNC_FLUSH_COMMANDS_BIT        0x00000001

/* The sync-object types under our own names: the OpenGL headers of 10.6 do not declare them (sync objects came
 * with a later OpenGL version), and these are the same pointer and 64-bit integer every later header declares. */
typedef struct nm_gl_sync_object *nm_glsync;
typedef unsigned long long nm_gluint64;

/* Mesa's API-layer entry stubs (they act on the thread's current Mesa context). */
extern void _dispatch_stub_Flush(void);
extern void _dispatch_stub_Finish(void);
extern nm_glsync _dispatch_stub_FenceSync(GLenum condition, GLbitfield flags);
extern GLenum _dispatch_stub_ClientWaitSync(nm_glsync sync, GLbitfield flags, nm_gluint64 wait_ns);
extern void _dispatch_stub_DeleteSync(nm_glsync sync);

/* 1,024 live fences is far above what any caller read so far keeps (the window server keeps one per display). */
#define NM_MAX_FENCES 1024
static struct { unsigned char used; nm_glsync sync; } nm_fence[NM_MAX_FENCES];
static pthread_mutex_t nm_fence_lock = PTHREAD_MUTEX_INITIALIZER;

void nm_apple_flush_render_APPLE(GLIContext ctx)  { nm_gl_enter(ctx); _dispatch_stub_Flush(); }
void nm_apple_finish_render_APPLE(GLIContext ctx) { nm_gl_enter(ctx); _dispatch_stub_Finish(); }
/* Presentation belongs to the drawable layer; as a table entry this is a flush. */
void nm_apple_swap_APPLE(GLIContext ctx)          { nm_gl_enter(ctx); _dispatch_stub_Flush(); }

void nm_apple_gen_fences_APPLE(GLIContext ctx, GLsizei n, GLuint *fences)
{
    (void)ctx;
    if (n <= 0 || !fences) return;
    pthread_mutex_lock(&nm_fence_lock);
    GLsizei got = 0;
    for (unsigned i = 1; i < NM_MAX_FENCES && got < n; i++)   /* name 0 is never a fence */
        if (!nm_fence[i].used) { nm_fence[i].used = 1; nm_fence[i].sync = NULL; fences[got++] = i; }
    pthread_mutex_unlock(&nm_fence_lock);
    for (; got < n; got++) fences[got] = 0;                    /* table full: the caller sees "not a fence" */
}

void nm_apple_delete_fences_APPLE(GLIContext ctx, GLsizei n, const GLuint *fences)
{
    if (n <= 0 || !fences) return;
    nm_gl_enter(ctx);
    for (GLsizei k = 0; k < n; k++) {
        GLuint f = fences[k];
        if (f == 0 || f >= NM_MAX_FENCES) continue;
        pthread_mutex_lock(&nm_fence_lock);
        nm_glsync s = nm_fence[f].used ? nm_fence[f].sync : NULL;
        nm_fence[f].used = 0; nm_fence[f].sync = NULL;
        pthread_mutex_unlock(&nm_fence_lock);
        if (s) _dispatch_stub_DeleteSync(s);
    }
}

GLboolean nm_apple_is_fence_APPLE(GLIContext ctx, GLuint fence)
{
    (void)ctx;
    if (fence == 0 || fence >= NM_MAX_FENCES) return 0;
    pthread_mutex_lock(&nm_fence_lock);
    GLboolean r = nm_fence[fence].used != 0;
    pthread_mutex_unlock(&nm_fence_lock);
    return r;
}

void nm_apple_set_fence_APPLE(GLIContext ctx, GLuint fence)
{
    if (!nm_apple_is_fence_APPLE(ctx, fence)) { nm_gl_refuse(ctx, "set_fence_APPLE: not a fence"); return; }
    nm_gl_enter(ctx);
    nm_glsync s = _dispatch_stub_FenceSync(GL_SYNC_GPU_COMMANDS_COMPLETE, 0);
    pthread_mutex_lock(&nm_fence_lock);
    nm_glsync old = nm_fence[fence].sync;
    nm_fence[fence].sync = s;
    pthread_mutex_unlock(&nm_fence_lock);
    if (old) _dispatch_stub_DeleteSync(old);
}

/* A wait of 0 tests, a long wait finishes. A fence that was never set counts as passed, as the extension says. */
static GLboolean nm_fence_wait(GLIContext ctx, GLuint fence, nm_gluint64 wait_ns)
{
    if (fence == 0 || fence >= NM_MAX_FENCES) return 1;
    pthread_mutex_lock(&nm_fence_lock);
    nm_glsync s = nm_fence[fence].used ? nm_fence[fence].sync : NULL;
    pthread_mutex_unlock(&nm_fence_lock);
    if (!s) return 1;
    nm_gl_enter(ctx);
    GLenum r = _dispatch_stub_ClientWaitSync(s, GL_SYNC_FLUSH_COMMANDS_BIT, wait_ns);
    return r == GL_ALREADY_SIGNALED || r == GL_CONDITION_SATISFIED;
}

GLboolean nm_apple_test_fence_APPLE(GLIContext ctx, GLuint fence) { return nm_fence_wait(ctx, fence, 0); }

void nm_apple_finish_fence_APPLE(GLIContext ctx, GLuint fence)
{
    /* one second per wait, at most thirty: a wait that never returns would hang the caller with no way to tell why */
    for (int i = 0; i < 30 && !nm_fence_wait(ctx, fence, 1000000000ull); i++) {}
}

/* Object tests: for a fence, its state; for a texture or any other object, work that touches it has been issued
 * by the time the caller asks, so a flush makes "complete soon" true and finish makes it true now. */
GLboolean nm_apple_test_object_APPLE(GLIContext ctx, GLenum object, GLuint name)
{
    if (object == GL_FENCE_APPLE) return nm_apple_test_fence_APPLE(ctx, name);
    nm_gl_enter(ctx);
    _dispatch_stub_Flush();
    return 1;
}

void nm_apple_finish_object_APPLE(GLIContext ctx, GLenum object, GLuint name)
{
    if (object == GL_FENCE_APPLE) { nm_apple_finish_fence_APPLE(ctx, name); return; }
    nm_gl_enter(ctx);
    _dispatch_stub_Finish();
}

/* Purgeable objects: we never discard a purgeable object's storage, so marking is accepted and the contents are
 * always reported as kept. That is a legal implementation: purging is permission, not obligation. */
GLenum nm_apple_object_purgeable_APPLE(GLIContext ctx, GLenum type, GLuint name, GLenum option)
{
    (void)ctx; (void)type; (void)name; (void)option;
    return GL_RETAINED_APPLE;
}
GLenum nm_apple_object_unpurgeable_APPLE(GLIContext ctx, GLenum type, GLuint name, GLenum option)
{
    (void)ctx; (void)type; (void)name; (void)option;
    return GL_RETAINED_APPLE;
}
void nm_apple_get_object_parameteriv_APPLE(GLIContext ctx, GLenum type, GLuint name, GLenum pname, GLint *params)
{
    (void)ctx; (void)type; (void)name; (void)pname;
    if (params) *params = 0;   /* GL_PURGEABLE_APPLE: false, nothing is ever in the purgeable state here */
}

/* Texture range: a hint that a span of client memory will hold texture data. The last value set is remembered so
 * the query returns it; it changes nothing else. */
static struct { GLsizei length; const GLvoid *pointer; } nm_range;
void nm_apple_texture_range_APPLE(GLIContext ctx, GLenum target, GLsizei length, const GLvoid *pointer)
{
    (void)ctx; (void)target;
    pthread_mutex_lock(&nm_fence_lock);
    nm_range.length = length; nm_range.pointer = pointer;
    pthread_mutex_unlock(&nm_fence_lock);
}
void nm_apple_get_tex_parameter_pointerv_APPLE(GLIContext ctx, GLenum target, GLenum pname, GLvoid **params)
{
    (void)ctx; (void)target; (void)pname;
    if (!params) return;
    pthread_mutex_lock(&nm_fence_lock);
    *params = (GLvoid *)nm_range.pointer;
    pthread_mutex_unlock(&nm_fence_lock);
}

/* A debugging label that names the process a window-server object belongs to. Nothing reads it here. */
void nm_apple_label_object_with_responsible_process_APPLE(GLIContext ctx, GLenum type, GLuint name, GLint pid)
{
    (void)ctx; (void)type; (void)name; (void)pid;
}
