/*
 * no_engine_test.c: the failure case. With no original engine to open, the bundle must make both loaders give up
 * cleanly: gliGetVersion answers 0 and no entry crashes.
 */
#include <dlfcn.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

typedef intptr_t (*fn8)(intptr_t, intptr_t, intptr_t, intptr_t, intptr_t, intptr_t, intptr_t, intptr_t);
int main(int argc, char **argv)
{
    if (argc < 2) return 2;
    setenv("NM_GL_APPLE_ENGINE", "/nonexistent/engine", 1);
    void *e = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
    if (!e) { fprintf(stderr, "cannot open: %s\n", dlerror()); return 2; }
    int ma = 9, mi = 9, id = 9;
    int r = ((int (*)(int *, int *, int *))dlsym(e, "gliGetVersion"))(&ma, &mi, &id);
    intptr_t q = ((fn8)dlsym(e, "gliQueryRendererInfo"))(0, 0, 0, 0, 0, 0, 0, 0);
    intptr_t s = ((intptr_t (*)(intptr_t, int, const void *))dlsym(e, "gliSetInteger"))(0, 222, &ma);
    int ok = r == 0 && q == 0 && s == 10004;
    printf("%s  with no original engine gliGetVersion answers 0 and the other entries fail without crashing  [version %d, query %ld, set %ld]\n",
           ok ? "PASS" : "FAIL", r, (long)q, (long)s);
    return !ok;
}
