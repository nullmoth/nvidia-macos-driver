/*
 * fake_engine.c: stands in for Apple's engine in the host test. It exports the same entry names, records what it
 * was called with, and answers with fixed values, so the test can check that the bundle under test forwards every
 * argument and return value unchanged.
 */
#include <stdint.h>
#include <string.h>

#define FAKE_EXPORT __attribute__((visibility("default")))

FAKE_EXPORT struct { char name[40]; intptr_t a[8]; int calls; } fake_last;
FAKE_EXPORT uint32_t fake_table_size = 7792; /* 974 slots of 8 bytes: the 10.11 table */

static intptr_t note(const char *n, intptr_t a0, intptr_t a1, intptr_t a2, intptr_t a3, intptr_t a4, intptr_t a5,
                     intptr_t a6, intptr_t a7)
{
    strncpy(fake_last.name, n, sizeof fake_last.name - 1);
    intptr_t v[8] = { a0, a1, a2, a3, a4, a5, a6, a7 };
    memcpy(fake_last.a, v, sizeof v);
    fake_last.calls++;
    return 0x5a5a0000 + fake_last.calls;
}
#define FAKE(n) FAKE_EXPORT intptr_t n(intptr_t a0, intptr_t a1, intptr_t a2, intptr_t a3, intptr_t a4, intptr_t a5, \
                                       intptr_t a6, intptr_t a7) { return note(#n, a0, a1, a2, a3, a4, a5, a6, a7); }
FAKE(gliInitializeLibrary) FAKE(gliTerminateLibrary) FAKE(gliQueryRendererInfo) FAKE(gliDestroyRendererInfo)
FAKE(gliChoosePixelFormat) FAKE(gliDestroyPixelFormat) FAKE(gliCreateContext) FAKE(gliCreateContextWithShared)
FAKE(gliDestroyContext) FAKE(gliAttachDrawableWithOptions) FAKE(gliSetInteger) FAKE(gliGetAttribute)
FAKE(gliSetAttribute) FAKE(gliCopyAttributes) FAKE(gliSwapBuffers)
/* gliAttachDrawable and gliGetMemoryAnnotationList are left out on purpose: the first must be served by the
 * bundle itself through the five-argument form, the second models the 10.7 engine, which does not have it. */

FAKE_EXPORT int gliGetVersion(int *major, int *minor, int *id)
{
    note("gliGetVersion", (intptr_t)major, (intptr_t)minor, (intptr_t)id, 0, 0, 0, 0, 0);
    *major = 2; *minor = 3; *id = 0x1234;
    return 1;
}
FAKE_EXPORT intptr_t gliGetInteger(intptr_t ctx, int pname, uint32_t *value)
{
    note("gliGetInteger", ctx, pname, (intptr_t)value, 0, 0, 0, 0, 0);
    if (pname == 224) { *value = fake_table_size; return 0; }
    return 10008;
}
