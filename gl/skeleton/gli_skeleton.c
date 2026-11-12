/*
 * gli_skeleton.c: export skeleton of an engine bundle.
 *
 * Read from the OpenGL framework binary, 10.7.5 to 10.13.6:
 *   - the framework lists its Resources folder (or the folder named by the GL_RESOURCES environment variable) and
 *     treats every folder whose name starts with "GLEngine" and whose extension is ".bundle" as an engine;
 *   - per engine it calls gliInitializeLibrary, then gliGetVersion, and resolves the names in gli_names_<era>.h;
 *   - the version check requires major 2 and minor 3 on every one of those releases, and a third value that no
 *     other loaded engine reported.
 * The window server does not go through that scan: it opens
 * /System/Library/Frameworks/OpenGL.framework/Resources/GLEngine.bundle/GLEngine by absolute path and resolves
 * twelve of the same names itself.
 *
 * An engine owns the whole OpenGL API of a context: it fills the function table declared in the public header
 * OpenGL/gliDispatch.h, which the framework stores beside the opaque engine context in the public CGL context
 * structure (OpenGL/CGLContext.h).
 */
#include "nm_gl_stub.h"

/* gliGetVersion: three pointer arguments (major, minor, engine identifier). The skeleton returns 0 to decline. */
__attribute__((visibility("default")))
int gliGetVersion(int *major, int *minor, int *engine_id)
{
    if (major) *major = 2;
    if (minor) *minor = 3;
    if (engine_id) *engine_id = 0;
    return 0;
}

#define GLI(name) NM_GL_STUB(name)
#define GLI_TYPED(name) NM_GL_TYPED(name)
#include NM_GLI_NAMES
