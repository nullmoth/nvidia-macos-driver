/* test_tgp_format: the power-budget lines of nvmtl-smi. cc -I. test_tgp_format.c -o /tmp/test_tgp_format && /tmp/test_tgp_format */
#include <string.h>
#include "tgp_format.h"
static int fails;
static void arm(const char *n, int ok) { printf("%s  %s\n", ok ? "ok  " : "FAIL", n); fails += !ok; }
static void run(const struct tgp_view *v, char *buf, size_t n) { FILE *f = fmemopen(buf, n, "w"); tgp_format(v, f); fclose(f); }
int main(void)
{
    static char buf[1024];
    struct tgp_view v = { 0, 0, 0, 0, 0, 0 };
    run(&v, buf, sizeof buf);
    arm("without the sysctls it explains the opt-in and laptop requirement without readings", strstr(buf, "tgp_mode inactive (requires nvrmtgp=1 and a detected laptop battery)") && !strstr(buf, "controls_sent"));
    v = (struct tgp_view){ 1, 1, 1, 1, 0, 1 };
    run(&v, buf, sizeof buf);
    arm("on mains with the mode sent reads on / mains", strstr(buf, "tgp_mode on") && strstr(buf, "power source mains") && strstr(buf, "controls_sent 1 last_status 0x0"));
    v = (struct tgp_view){ 1, 1, 0, 0, 0, 2 };
    run(&v, buf, sizeof buf);
    arm("after the charger went away it reads off / battery", strstr(buf, "tgp_mode off") && strstr(buf, "power source battery"));
    v = (struct tgp_view){ 1, 1, -1, -1, 0, 0 };
    run(&v, buf, sizeof buf);
    arm("before any reading it says not sent yet and unknown", strstr(buf, "not sent yet") && strstr(buf, "unknown"));
    v = (struct tgp_view){ 1, 0, 0, 1, 0x1f, 3 };
    run(&v, buf, sizeof buf);
    arm("the live rollback shows as forced off, and a refused control shows its status", strstr(buf, "no (forced off)") && strstr(buf, "last_status 0x1f"));
    printf(fails ? "FAILED %d\n" : "ok\n", fails);
    return fails != 0;
}
