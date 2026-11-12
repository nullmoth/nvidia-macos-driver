/*
 * apple_ext_test.c: host test of the Apple-extension entries against stand-ins for Mesa's entry stubs.
 * The stand-ins count calls and let the test decide when a sync object is "signalled".
 */
#include <OpenGL/gliDispatch.h>
#include <stdio.h>

static int flushes, finishes, syncs_made, syncs_deleted, enters, refusals, signalled;
void nm_gl_enter(GLIContext c) { (void)c; enters++; }
void nm_gl_refuse(GLIContext c, const char *e) { (void)c; (void)e; refusals++; }
void _dispatch_stub_Flush(void) { flushes++; }
void _dispatch_stub_Finish(void) { finishes++; }
void *_dispatch_stub_FenceSync(GLenum c, GLbitfield f) { (void)c; (void)f; return (void *)(long)(0x1000 + ++syncs_made); }
GLenum _dispatch_stub_ClientWaitSync(void *s, GLbitfield f, unsigned long long t) { (void)s; (void)f; (void)t; return signalled ? 0x911A : 0x911B; }
void _dispatch_stub_DeleteSync(void *s) { (void)s; syncs_deleted++; }

void nm_apple_flush_render_APPLE(GLIContext);
void nm_apple_gen_fences_APPLE(GLIContext, GLsizei, GLuint *);
void nm_apple_delete_fences_APPLE(GLIContext, GLsizei, const GLuint *);
void nm_apple_set_fence_APPLE(GLIContext, GLuint);
GLboolean nm_apple_is_fence_APPLE(GLIContext, GLuint);
GLboolean nm_apple_test_fence_APPLE(GLIContext, GLuint);
GLboolean nm_apple_test_object_APPLE(GLIContext, GLenum, GLuint);
void nm_apple_finish_object_APPLE(GLIContext, GLenum, GLuint);
GLenum nm_apple_object_purgeable_APPLE(GLIContext, GLenum, GLuint, GLenum);
void nm_apple_texture_range_APPLE(GLIContext, GLenum, GLsizei, const GLvoid *);
void nm_apple_get_tex_parameter_pointerv_APPLE(GLIContext, GLenum, GLenum, GLvoid **);

static int failed, arms;
static void arm(int ok, const char *name, long a, long b) { arms++; failed += !ok; printf("%s  %s  [%ld, %ld]\n", ok ? "PASS" : "FAIL", name, a, b); }

int main(void)
{
    GLIContext c = (GLIContext)0x10;
    nm_apple_flush_render_APPLE(c);
    arm(flushes == 1 && enters == 1, "flush-render makes the passed context current and flushes once", flushes, enters);

    GLuint f[2] = { 0, 0 };
    nm_apple_gen_fences_APPLE(c, 2, f);
    arm(f[0] != 0 && f[1] != 0 && f[0] != f[1], "two generated fences have distinct non-zero names", f[0], f[1]);
    arm(nm_apple_is_fence_APPLE(c, f[0]) && !nm_apple_is_fence_APPLE(c, 999), "a generated name is a fence and an arbitrary name is not", nm_apple_is_fence_APPLE(c, f[0]), nm_apple_is_fence_APPLE(c, 999));
    arm(nm_apple_test_fence_APPLE(c, f[0]) == 1, "a fence that was never set tests as passed", nm_apple_test_fence_APPLE(c, f[0]), 0);

    nm_apple_set_fence_APPLE(c, f[0]);
    signalled = 0;
    arm(syncs_made == 1 && nm_apple_test_fence_APPLE(c, f[0]) == 0, "a set fence whose work is still pending tests as NOT passed", syncs_made, nm_apple_test_fence_APPLE(c, f[0]));
    signalled = 1;
    arm(nm_apple_test_fence_APPLE(c, f[0]) == 1 && nm_apple_test_object_APPLE(c, 0x8A0B, f[0]) == 1, "the same fence tests as passed once its work is done, also through the object test", nm_apple_test_fence_APPLE(c, f[0]), 1);

    nm_apple_set_fence_APPLE(c, f[0]);
    arm(syncs_made == 2 && syncs_deleted == 1, "setting a fence again replaces its sync object and deletes the old one", syncs_made, syncs_deleted);
    int r0 = refusals; nm_apple_set_fence_APPLE(c, 777);
    arm(refusals == r0 + 1 && syncs_made == 2, "setting a name that is not a fence is refused and creates nothing", refusals - r0, syncs_made);

    nm_apple_delete_fences_APPLE(c, 2, f);
    arm(syncs_deleted == 2 && !nm_apple_is_fence_APPLE(c, f[0]), "deleting fences deletes the live sync object and frees the names", syncs_deleted, nm_apple_is_fence_APPLE(c, f[0]));

    int fin = finishes; nm_apple_finish_object_APPLE(c, 0x1702 /* GL_TEXTURE */, 5);
    arm(finishes == fin + 1, "finishing a texture object finishes the context once", finishes - fin, 0);
    arm(nm_apple_object_purgeable_APPLE(c, 0x1702, 5, 0x8A1D) == 0x8A1B, "a purgeable request reports the contents as retained", nm_apple_object_purgeable_APPLE(c, 0x1702, 5, 0x8A1D), 0x8A1B);

    static const char mem[16]; GLvoid *p = 0;
    nm_apple_texture_range_APPLE(c, 0x84F5, sizeof mem, mem);
    nm_apple_get_tex_parameter_pointerv_APPLE(c, 0x84F5, 0x85B8, &p);
    arm(p == (GLvoid *)mem, "the texture-range pointer that was set is the one returned", p == (GLvoid *)mem, 1);

    printf("%d arms, %d failed\n", arms, failed);
    return failed != 0;
}
