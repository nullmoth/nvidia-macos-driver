/* backlight(9) registry for the port (see dev/backlight/backlight.h). */
#include <sys/param.h>
#include <sys/systm.h>
#include <sys/malloc.h>
#include <sys/lock.h>
#include <sys/mutex.h>
#include <sys/queue.h>
#include <sys/conf.h>
#include <dev/backlight/backlight.h>

MALLOC_DEFINE(M_NMXBL, "nmxbacklight", "backlight records");

static TAILQ_HEAD(, cdev) backlights = TAILQ_HEAD_INITIALIZER(backlights);
static struct mtx bl_mtx;
MTX_SYSINIT(nmx_backlight, &bl_mtx, "backlights", MTX_DEF);

struct cdev *backlight_register(const char *name, device_t dev)
{
    struct cdev *c = malloc(sizeof(*c), M_NMXBL, M_WAITOK | M_ZERO);
    if (c == NULL)
        return NULL;
    snprintf(c->si_name, sizeof(c->si_name), "backlight/%s", name ? name : "panel");
    c->si_drv1 = dev;
    mtx_lock(&bl_mtx);
    TAILQ_INSERT_TAIL(&backlights, c, nmx_link);
    mtx_unlock(&bl_mtx);
    return c;
}

int backlight_destroy(struct cdev *c)
{
    if (c == NULL)
        return 0;
    mtx_lock(&bl_mtx);
    TAILQ_REMOVE(&backlights, c, nmx_link);
    mtx_unlock(&bl_mtx);
    free(c, M_NMXBL);
    return 0;
}

struct cdev *nmx_backlight_next(struct cdev *prev, device_t *devp)
{
    mtx_lock(&bl_mtx);
    struct cdev *c = prev ? TAILQ_NEXT(prev, nmx_link) : TAILQ_FIRST(&backlights);
    if (c && devp)
        *devp = c->si_drv1;
    mtx_unlock(&bl_mtx);
    return c;
}
