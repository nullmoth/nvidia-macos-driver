/* Per-process GPU choice on hybrid laptops (plugin/nvmtl_gpu_policy.h), on the host.
 *   cc -Wall -I../plugin test_gpu_policy.c -o /tmp/t && /tmp/t     -> 0 failed
 *   cc -DALWAYS_NVIDIA ...   (the plugin before the policy: every process gets the device)  -> the "integrated GPU" arms fail */
#include <stdio.h>
#include "nvmtl_gpu_policy.h"
static int fails;
#define ARM(name, cond) do { if (cond) printf("ok   %s\n", name); else { printf("FAIL %s\n", name); fails++; } } while (0)
#ifdef ALWAYS_NVIDIA
#define DECIDE(h, m, p, a, o) ((void)(h), (void)(m), (void)(p), (void)(a), (void)(o), 1)
#else
#define DECIDE(h, m, p, a, o) nmp_decide(h, m, p, a, o, 0)
#endif
int main(void)
{
    const char *file = "# policy\nmode apple\nigpu Safari\nnvidia Blender  \nigpu Cyberpunk2077\nnvidia Cyberpunk2077\n\n";
    int apple;
    ARM("a process with no rule gets AUTO", nmp_parse(file, "Notes", &apple) == NMP_PREF_AUTO);
    ARM("mode apple is read", apple == 1);
    ARM("igpu rule", nmp_parse(file, "Safari", &apple) == NMP_PREF_IGPU);
    ARM("nvidia rule, trailing blanks ignored", nmp_parse(file, "Blender", &apple) == NMP_PREF_NVIDIA);
    ARM("the last matching rule wins", nmp_parse(file, "Cyberpunk2077", &apple) == NMP_PREF_NVIDIA);
    ARM("a prefix of a name does not match", nmp_parse(file, "Safar", &apple) == NMP_PREF_AUTO);
    ARM("no file means mode nvidia", (nmp_parse(NULL, "x", &apple), apple == 0));
    ARM("mode nvidia after mode apple switches back", (nmp_parse("mode apple\nmode nvidia\n", "x", &apple), apple == 0));

    ARM("switch off: every process gets the NVIDIA device", DECIDE(0, 1, NMP_PREF_IGPU, 1, 1) == 1);
    ARM("no other Metal GPU: the device is given even if the policy says integrated", DECIDE(1, 1, NMP_PREF_IGPU, 1, 0) == 1);
    ARM("a per-process igpu rule hides the device", DECIDE(1, 0, NMP_PREF_IGPU, -1, 1) == 0);
    ARM("a per-process nvidia rule gives it even in mode apple for an app that allows switching", DECIDE(1, 1, NMP_PREF_NVIDIA, 1, 1) == 1);
    ARM("default mode gives the device to everyone", DECIDE(1, 0, NMP_PREF_AUTO, 1, 1) == 1);
    ARM("mode apple: an app that allows automatic switching is sent to the integrated GPU", DECIDE(1, 1, NMP_PREF_AUTO, 1, 1) == 0);
    ARM("mode apple: an app without the key keeps the high-performance GPU", DECIDE(1, 1, NMP_PREF_AUTO, -1, 1) == 1);
    ARM("mode apple: an app that sets the key to false keeps it", DECIDE(1, 1, NMP_PREF_AUTO, 0, 1) == 1);
    const char *w = 0; nmp_decide(1, 1, NMP_PREF_AUTO, 1, 1, &w);
    ARM("the reason is named for the log", w && strstr(w, "automatic graphics switching"));
    printf("%d failed\n", fails);
    return fails != 0;
}
