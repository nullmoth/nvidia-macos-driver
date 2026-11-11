/*
 * engine_test.c: host test of the engine bundle against a stand-in for Apple's engine.
 * Arms are named sentences; each prints PASS or FAIL and the values it compared. The test follows the window
 * server's measured start-up order and then checks the log the bundle wrote.
 */
#include "../nm_gli.h"

#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

typedef intptr_t (*fn8)(intptr_t, intptr_t, intptr_t, intptr_t, intptr_t, intptr_t, intptr_t, intptr_t);
static int failed, arms;
static void arm(int ok, const char *name, const char *detail)
{
    arms++; failed += !ok;
    printf("%s  %s  [%s]\n", ok ? "PASS" : "FAIL", name, detail);
}

int main(int argc, char **argv)
{
    if (argc < 4) { fprintf(stderr, "usage: engine_test <engine bundle> <fake engine> <log file>\n"); return 2; }
    const char *log = argv[3];
    unlink(log);
    setenv("NM_GL_APPLE_ENGINE", argv[2], 1);
    setenv("NM_GL_LOG", log, 1);
    void *e = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL), *fk = dlopen(argv[2], RTLD_NOW | RTLD_LOCAL);
    if (!e || !fk) { fprintf(stderr, "cannot open: %s\n", dlerror()); return 2; }
    struct { char name[40]; intptr_t a[8]; int calls; } *last = dlsym(fk, "fake_last");
    char d[256];

    /* 1. gliInitializeLibrary: seven arguments, the seventh on the stack */
    fn8 init = (fn8)dlsym(e, "gliInitializeLibrary");
    intptr_t r = init(11, 22, 33, 44, 55, 66, 77, 0);
    int ok = !strcmp(last->name, "gliInitializeLibrary");
    for (int i = 0; i < 7; i++) ok &= last->a[i] == 11 * (i + 1);
    snprintf(d, sizeof d, "callee saw %ld %ld %ld %ld %ld %ld %ld, returned %#lx", (long)last->a[0], (long)last->a[1],
             (long)last->a[2], (long)last->a[3], (long)last->a[4], (long)last->a[5], (long)last->a[6], (long)r);
    arm(ok && r == 0x5a5a0001, "all seven arguments of gliInitializeLibrary and its return value pass through unchanged", d);

    /* 2. version */
    int (*ver)(int *, int *, int *) = (int (*)(int *, int *, int *))dlsym(e, "gliGetVersion");
    int ma = 0, mi = 0, id = 0, vr = ver(&ma, &mi, &id);
    snprintf(d, sizeof d, "returned %d, version %d.%d, id %#x", vr, ma, mi, id);
    arm(vr == 1 && ma == NM_GLI_VERSION_MAJOR && mi == NM_GLI_VERSION_MINOR && id == 0x1234,
        "gliGetVersion answers the 2.3 both loaders require and keeps the engine identifier", d);

    /* 3. table size through the public parameter, with a null context as the window server asks */
    intptr_t (*geti)(intptr_t, int, void *) = (intptr_t (*)(intptr_t, int, void *))dlsym(e, "gliGetInteger");
    uint32_t size = 0; r = geti(0, NM_CP_DISPATCH_TABLE_SIZE, &size);
    snprintf(d, sizeof d, "returned %ld, size %u", (long)r, size);
    arm(r == 0 && size == 7792, "the function-table size query with a null context returns the engine's size", d);

    /* 4. a parameter the stand-in refuses: the error code must come back, not be swallowed */
    uint32_t v = 0; r = geti(0x1000, NM_CP_QUERY_320, &v);
    snprintf(d, sizeof d, "returned %ld", (long)r);
    arm(r == 10008, "an error code from the original engine is returned to the caller unchanged", d);

    /* 5. set-integer with the IOSurface texture block */
    intptr_t (*seti)(intptr_t, int, const void *) = (intptr_t (*)(intptr_t, int, const void *))dlsym(e, "gliSetInteger");
    nm_tex_iosurface io = { 0x77, 0x84f5, 0x1908, 640, 480, 0x80e1, 0x8367, 0 };
    r = seti(0x2000, NM_CP_TEX_IOSURFACE, &io);
    snprintf(d, sizeof d, "callee saw ctx %#lx pname %ld pointer %s", (long)last->a[0], (long)last->a[1],
             last->a[2] == (intptr_t)&io ? "same" : "DIFFERENT");
    arm(!strcmp(last->name, "gliSetInteger") && last->a[0] == 0x2000 && last->a[1] == 910 && last->a[2] == (intptr_t)&io,
        "gliSetInteger forwards context, parameter number and the caller's own block pointer", d);

    /* 6. four-argument attach is served through the five-argument entry with options 0 */
    intptr_t (*att)(intptr_t, int, const void *, int) = (intptr_t (*)(intptr_t, int, const void *, int))dlsym(e, "gliAttachDrawable");
    uint64_t desc[3] = { 1, 2, 0xabc };
    att(0x3000, NM_DRAWABLE_SURFACE_UNFLIP, desc, 2);
    snprintf(d, sizeof d, "callee %s saw type %#lx screen %ld options %ld", last->name, (long)last->a[1], (long)last->a[3], (long)last->a[4]);
    arm(!strcmp(last->name, "gliAttachDrawableWithOptions") && last->a[1] == 0x5a && last->a[3] == 2 && last->a[4] == 0,
        "gliAttachDrawable reaches the original engine as the five-argument form with options 0", d);

    /* 7. an entry the original engine does not have answers 0 and does not crash */
    fn8 ann = (fn8)dlsym(e, "gliGetMemoryAnnotationList");
    int before = last->calls; r = ann ? ann(0, 0, 0, 0, 0, 0, 0, 0) : -1;
    snprintf(d, sizeof d, "returned %ld, original engine called %d more times", (long)r, last->calls - before);
    arm(ann && r == 0 && last->calls == before, "an entry missing from the original engine answers 0 without calling into it", d);

    /* 8. the log: every call above is there, with the parameter numbers and the block bytes */
    FILE *f = fopen(log, "r"); char line[2048]; int lines = 0, has910 = 0, has224 = 0, hasatt = 0, hasinit = 0, hasblock = 0;
    while (f && fgets(line, sizeof line, f)) {
        lines++;
        has910 |= strstr(line, "gliSetInteger") && strstr(line, "pname=910") != NULL;
        hasblock |= strstr(line, "block=32:77000000f5840000") != NULL;
        has224 |= strstr(line, "gliGetInteger") && strstr(line, "pname=224") != NULL;
        hasatt |= strstr(line, "type=0x5a desc=24:0100000000000000") != NULL;
        hasinit |= strstr(line, "gliInitializeLibrary(") != NULL;
    }
    if (f) fclose(f);
    snprintf(d, sizeof d, "%d lines; init %d, 224 %d, 910 %d, block bytes %d, attach %d", lines, hasinit, has224, has910, hasblock, hasatt);
    arm(lines >= 8 && hasinit && has224 && has910 && hasblock && hasatt,
        "the log holds every entry call, each parameter number, and the bytes of the argument block and descriptor", d);

    /* 9. negative control for the log check: a string that was never logged must not be found */
    f = fopen(log, "r"); int bogus = 0;
    while (f && fgets(line, sizeof line, f)) bogus |= strstr(line, "pname=4242") != NULL;
    if (f) fclose(f);
    arm(!bogus, "control: a parameter number that was never passed is not in the log", bogus ? "found" : "absent");

    printf("%d arms, %d failed\n", arms, failed);
    return failed != 0;
}
