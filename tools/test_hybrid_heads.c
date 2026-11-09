/* The framebuffer lifecycle for the integrated GPU's heads (kexts/NVRM/accel/nm_hybrid_heads.h): one device shared by the panel and the external
 * heads, publish after bring-up, unpublish before take-down, replug during teardown, stop order.
 *   cc -O1 -Wall -I../kexts/NVRM/accel test_hybrid_heads.c -o /tmp/t && /tmp/t     -> 0 failed */
#include <stdio.h>
#include <stdlib.h>
#include "nm_hybrid_heads.h"
static int fails;
#define ARM(name, cond) do { if (cond) printf("ok   %s\n", name); else { printf("FAIL %s\n", name); fails++; } } while (0)

static char logbuf[4096]; static int logn;
static int objs, dev_open_n, dev_close_n, dev_is_open, fail_create, fail_up, fail_publish, fail_dev;
static struct nmhd D; static struct nmhd_ops OPS;
static int replug_in_takedown = -1, flap_in_takedown = -1;       /* a port whose plug arrives while a head is being taken down */
static void L(const char *s) { logn += snprintf(logbuf + logn, sizeof logbuf - (unsigned)logn, "%s ", s); }
static int has(const char *s) { return strstr(logbuf, s) != 0; }
static int before(const char *a, const char *b) { const char *x = strstr(logbuf, a), *y = strstr(logbuf, b); return x && y && x < y; }

static int dev_open(void *c) { (void)c; if (fail_dev) return -1; dev_open_n++; dev_is_open = 1; L("open"); return 0; }
static void dev_close(void *c) { (void)c; dev_close_n++; dev_is_open = 0; L("close"); }
static void *create(void *c, unsigned slot, unsigned port) { (void)c; (void)slot; (void)port; if (fail_create || !dev_is_open) return 0; objs++; L("create"); return malloc(8); }
static int bring_up(void *c, void *o, unsigned port) { (void)c; (void)o; (void)port; if (fail_up) return -1; L("up"); return 0; }
static int publish(void *c, void *o, unsigned slot) { (void)c; (void)o; (void)slot; if (fail_publish) return -1; L("publish"); return 0; }
static void unpublish(void *c, void *o) { (void)c; (void)o; L("unpublish"); }
static void take_down(void *c, void *o, unsigned port) { (void)c; (void)o; L("down"); if (replug_in_takedown >= 0) { unsigned p = (unsigned)replug_in_takedown; replug_in_takedown = -1; nmhd_plug(&D, &OPS, p); } if (flap_in_takedown >= 0) { unsigned p = (unsigned)flap_in_takedown; flap_in_takedown = -1; nmhd_plug(&D, &OPS, p); nmhd_unplug(&D, &OPS, p); } (void)port; }
static void release(void *c, void *o) { (void)c; free(o); objs--; L("release"); }

static void reset(void)
{
    nmhd_init(&D); memset(logbuf, 0, sizeof logbuf); logn = 0; objs = dev_open_n = dev_close_n = dev_is_open = 0; fail_create = fail_up = fail_publish = fail_dev = 0; replug_in_takedown = flap_in_takedown = -1;
    OPS = (struct nmhd_ops){ 0, dev_open, dev_close, create, bring_up, publish, unpublish, take_down, release };
}

int main(void)
{
    reset();
    ARM("a plug with no panel up is refused (the device is not open)", nmhd_plug(&D, &OPS, 4) == -1 && objs == 0);
    ARM("the panel opens the device once", nmhd_panel_up(&D, &OPS) == 0 && dev_open_n == 1 && D.dev_users == 1);
    ARM("a plug creates, brings up and only then publishes", nmhd_plug(&D, &OPS, 4) == 0 && nmhd_count(&D) == 1 && before("create", "up") && before("up", "publish") && objs == 1);
    ARM("the device is shared: not opened again", dev_open_n == 1 && D.dev_users == 2);
    ARM("a second plug for the same port is a no-op", nmhd_plug(&D, &OPS, 4) == 0 && nmhd_count(&D) == 1 && objs == 1);
    ARM("a second port gets a second head and a second slot", nmhd_plug(&D, &OPS, 5) == 0 && nmhd_count(&D) == 2 && D.h[0].port == 4 && D.h[1].port == 5 && D.dev_users == 3);
    memset(logbuf, 0, sizeof logbuf); logn = 0;
    ARM("an unplug unpublishes, takes the display down, then releases the object", nmhd_unplug(&D, &OPS, 4) == 0 && before("unpublish", "down") && before("down", "release") && nmhd_count(&D) == 1 && objs == 1);
    ARM("the device stays open while another head or the panel uses it", dev_is_open && D.dev_users == 2 && dev_close_n == 0);
    ARM("an unplug of a port with no head is harmless", nmhd_unplug(&D, &OPS, 4) == 0 && objs == 1);
    ARM("the freed slot is reused", nmhd_plug(&D, &OPS, 6) == 0 && D.h[0].port == 6 && nmhd_count(&D) == 2);

    /* failures release what was created */
    nmhd_unplug(&D, &OPS, 5); nmhd_unplug(&D, &OPS, 6);
    fail_create = 1; ARM("a framebuffer that cannot be created leaves nothing behind", nmhd_plug(&D, &OPS, 4) == -1 && objs == 0 && D.dev_users == 1 && nmhd_count(&D) == 0); fail_create = 0;
    memset(logbuf, 0, sizeof logbuf); logn = 0;
    fail_up = 1; ARM("a display that will not come up is taken down and its object released, never published", nmhd_plug(&D, &OPS, 4) == -1 && objs == 0 && !has("publish") && has("down") && has("release") && D.dev_users == 1); fail_up = 0;
    memset(logbuf, 0, sizeof logbuf); logn = 0;
    fail_publish = 1; ARM("a head that will not publish is taken down and released", nmhd_plug(&D, &OPS, 4) == -1 && objs == 0 && before("up", "down") && D.dev_users == 1); fail_publish = 0;
    ARM("the next plug works after the failures", nmhd_plug(&D, &OPS, 4) == 0 && objs == 1);
    nmhd_unplug(&D, &OPS, 4);

    /* the table is full */
    { unsigned p, ok = 1; for (p = 0; p < NMHD_MAX; p++) ok &= nmhd_plug(&D, &OPS, 10 + p) == 0;
      ARM("four external heads at once", ok && nmhd_count(&D) == NMHD_MAX);
      ARM("a fifth is refused and leaves nothing behind", nmhd_plug(&D, &OPS, 20) == -1 && objs == NMHD_MAX && D.dev_users == 1 + NMHD_MAX);
      for (p = 0; p < NMHD_MAX; p++) nmhd_unplug(&D, &OPS, 10 + p); ARM("all unplugged", objs == 0 && D.dev_users == 1); }

    /* a plug that arrives while the same port is being taken down */
    nmhd_plug(&D, &OPS, 4); memset(logbuf, 0, sizeof logbuf); logn = 0;
    replug_in_takedown = 4;
    ARM("a replug during its own teardown waits, then brings the head up again after the teardown finished", nmhd_unplug(&D, &OPS, 4) == 0 && nmhd_count(&D) == 1 && D.h[0].port == 4 && before("release", "create") && objs == 1);
    ARM("the replay queue is empty afterwards", D.replug_n == 0);
    nmhd_plug(&D, &OPS, 5); flap_in_takedown = 5;
    ARM("a plug and an unplug of the same port during its teardown cancel each other: no head, nothing queued", nmhd_unplug(&D, &OPS, 5) == 0 && nmhd_find(&D, 5) < 0 && D.replug_n == 0 && nmhd_count(&D) == 1);
    nmhd_unplug(&D, &OPS, 4); nmhd_unplug(&D, &OPS, 5);

    /* stop */
    nmhd_plug(&D, &OPS, 7); nmhd_plug(&D, &OPS, 8); memset(logbuf, 0, sizeof logbuf); logn = 0;
    nmhd_stop(&D, &OPS);
    ARM("stop takes every external head down, newest first, then closes the device", objs == 0 && dev_is_open == 0 && D.dev_users == 0 && before("down", "close"));
    { const char *a = strstr(logbuf, "unpublish"); int n = 0; while (a) { n++; a = strstr(a + 1, "unpublish"); } ARM("two heads were unpublished before the device closed", n == 2); }
    ARM("a plug after stop is ignored", nmhd_plug(&D, &OPS, 9) == -1 && objs == 0);
    ARM("stop twice is harmless", ({ nmhd_stop(&D, &OPS); dev_close_n == dev_open_n; }));

    /* the device cannot be opened */
    reset(); fail_dev = 1; ARM("a device that will not open stops the panel's start", nmhd_panel_up(&D, &OPS) == -1 && D.dev_users == 0);
    /* the panel is stopped while heads are up */
    reset(); nmhd_panel_up(&D, &OPS); nmhd_plug(&D, &OPS, 4); nmhd_stop(&D, &OPS);
    ARM("stopping with a head up leaves no object and the device closed exactly once", objs == 0 && dev_close_n == 1 && dev_open_n == 1);
    printf("%d failed\n", fails);
    return fails != 0;
}
