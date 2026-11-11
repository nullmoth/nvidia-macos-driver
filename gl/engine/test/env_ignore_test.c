/*
 * env_ignore_test.c <engine bundle> <stand-in engine> <record file> <expect: ignored | honoured | nolog>
 * Sets both test variables, calls the entry that reads them (the first entry call opens the forward target), and
 * checks what happened from outside the bundle:
 *   ignored   (default build)        the stand-in was NOT loaded into the process and NO file was created
 *   honoured  (test build; control)  the stand-in WAS loaded and the record file exists: the check can see both
 *   nolog     (test build)           the record path is a link: the stand-in loads, the link target stays empty
 */
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(int argc, char **argv)
{
    if (argc < 5) return 2;
    const char *bundle = argv[1], *fake = argv[2], *rec = argv[3], *expect = argv[4];
    char target[1024]; snprintf(target, sizeof target, "%s.target", rec);
    unlink(rec); unlink(target);
    if (!strcmp(expect, "nolog")) {
        FILE *t = fopen(target, "w"); if (t) fclose(t);
        if (symlink(target, rec) != 0) { perror("symlink"); return 2; }
    }
    setenv("NM_GL_APPLE_ENGINE", fake, 1);
    setenv("NM_GL_LOG", rec, 1);
    void *e = dlopen(bundle, RTLD_NOW | RTLD_LOCAL);
    if (!e) { fprintf(stderr, "cannot open: %s\n", dlerror()); return 2; }
    int ma = 0, mi = 0, id = 0;
    int r = ((int (*)(int *, int *, int *))dlsym(e, "gliGetVersion"))(&ma, &mi, &id);
    /* RTLD_NOLOAD answers only for an image that is already in the process */
    int loaded = dlopen(fake, RTLD_NOLOAD | RTLD_LAZY) != NULL;
    struct stat st; int created = lstat(rec, &st) == 0;
    long target_size = stat(target, &st) == 0 ? (long)st.st_size : -1;
    int ok;
    if (!strcmp(expect, "ignored")) ok = !loaded && !created && r == 0;
    else if (!strcmp(expect, "honoured")) ok = loaded && created && r == 1;
    else ok = loaded && r == 1 && target_size == 0;
    const char *name = !strcmp(expect, "ignored") ? "a default build ignores both variables: the named engine is not loaded and no file is created"
                     : !strcmp(expect, "honoured") ? "control: a test build honours both variables, so the check can see a load and a file"
                     : "a test build refuses a record path that is a link: nothing is written through it";
    printf("%s  %s  [version call %d, stand-in loaded %d, record path exists %d, link target size %ld]\n",
           ok ? "PASS" : "FAIL", name, r, loaded, created, target_size);
    unlink(rec); unlink(target);
    return !ok;
}
