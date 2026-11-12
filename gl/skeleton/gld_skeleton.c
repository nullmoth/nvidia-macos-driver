/*
 * gld_skeleton.c: export skeleton of a vendor renderer bundle ("GLDriver").
 *
 * How the framework finds it (read from the 10.7.5 to 10.13.6 binaries of libGFXShared.dylib):
 *   - every IOFramebuffer is mapped to its accelerator service with IOAccelFindAccelerator();
 *   - the accelerator's registry property IOGLBundleName names the bundle, which is opened from
 *     /System/Library/Extensions/<name>.bundle/Contents/MacOS/<name>;
 *   - gldInitializeLibrary is called, then gldGetVersion, then every name in gld_names_<era>.h is resolved with
 *     dlsym(). The same list is what Apple's software renderer (GLRendererFloat) exports.
 *
 * Apple's own engine (GLEngine.bundle) sits above this interface and passes its private state structures through
 * it. Those structures are not public and differ per release, so the bodies cannot be written from the names alone.
 */
#include "nm_gl_stub.h"

/*
 * gldGetVersion, measured at the call site in libGFXShared (10.7.5): four pointer arguments; the loader requires a
 * non-zero return, major == 4, minor == 0 on 10.7 and 1 on 10.8 and later, a third value of 0x42 on 10.7, and a
 * renderer identifier with no bits outside 0x0000ff00, which it then combines with 0x00020000 and requires to be
 * different from every renderer already loaded. Identifier byte 0x04 belongs to the software renderer.
 *
 * The skeleton returns 0, which makes the loader disconnect the bundle.
 */
__attribute__((visibility("default")))
int gldGetVersion(int *major, int *minor, int *revision, int *renderer_id)
{
    if (major) *major = 4;
    if (minor) *minor = (NM_GL_ERA >= 1008) ? 1 : 0;
    if (revision) *revision = 0;
    if (renderer_id) *renderer_id = 0;
    return 0;
}

#define GLD(name) NM_GL_STUB(name)
#define GLD_TYPED(name) NM_GL_TYPED(name)
#include NM_GLD_NAMES
