/*
 * nm_gl_stub.h: shared pieces of the two export skeletons (renderer level and engine level).
 *
 * These files exist to pin down the entry-point sets that Apple's OpenGL framework resolves by name, release by
 * release, and to prove that bundles carrying those sets can be built for the oldest release (10.6) in both user
 * architectures. They contain no rendering code. Every entry point that has not been written reports itself once
 * and returns a failure value, and the version entry points decline the load, so a skeleton that is ever loaded by
 * mistake is rejected by the framework's own version check instead of being used.
 */
#ifndef NM_GL_STUB_H
#define NM_GL_STUB_H

#include <stdint.h>
#include <stdio.h>

/* NM_GL_ERA: release the bundle is built for, as major * 100 + minor (10.7 is 1007). Chosen on the command line. */
#ifndef NM_GL_ERA
#error "NM_GL_ERA must be set (for example -DNM_GL_ERA=1007)"
#endif

static inline intptr_t nm_gl_unimplemented(const char *name)
{
    /* stderr only: no state is kept, so this is safe from any thread and before any initialization */
    fprintf(stderr, "nm-gl skeleton: %s is not implemented\n", name);
    return 0;
}

/*
 * Six word-sized arguments cover every integer-register argument of the 64-bit calling convention. On i386 the
 * caller removes its own arguments, so a callee that declares more than it was given is harmless as long as it
 * does not read them. The real signatures are not public; the ones that have been measured are written out in the
 * source files with the measurement beside them.
 */
#define NM_GL_STUB(name)                                                                              \
    __attribute__((visibility("default")))                                                            \
    intptr_t name(intptr_t a0, intptr_t a1, intptr_t a2, intptr_t a3, intptr_t a4, intptr_t a5)       \
    {                                                                                                 \
        (void)a0; (void)a1; (void)a2; (void)a3; (void)a4; (void)a5;                                   \
        return nm_gl_unimplemented(#name);                                                            \
    }
#define NM_GL_TYPED(name) /* written out by hand in the including file */

#endif
