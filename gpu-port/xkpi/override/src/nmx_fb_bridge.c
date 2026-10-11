/*
 * The display bridge, DRM side: lights the GPU's outputs with one scan-out buffer that macOS draws into.
 *
 * It is an in-kernel DRM client, the same mechanism Linux's fbdev emulation uses: probe the connectors, pick each
 * output's preferred mode, create one XRGB8888 buffer as large as the largest mode, point every lit CRTC at it and
 * commit. The buffer's CPU view is the GPU aperture (a CPU-visible, contiguous VRAM buffer read through the BAR), so
 * the kext publishes its physical range as the IOFramebuffer's VRAM and WindowServer draws straight into what the
 * display engine scans out. Smaller outputs show the top-left of the buffer (mirrored), as fbdev does.
 *
 * Any step that fails leaves no client behind and reports why; the kext then publishes no framebuffer.
 */
#include <linux/kernel.h>
#include <linux/slab.h>
#include <linux/pci.h>
#include <linux/iosys-map.h>

#include <drm/drm_client.h>
#include <drm/drm_device.h>
#include <drm/drm_fourcc.h>
#include <drm/drm_framebuffer.h>
#include <drm/drm_modes.h>
#include <drm/drm_crtc.h>

#include <sys/bus.h>

#include "nmx_xnu.h"
#include "nmx_port.h"

struct nmx_fb {
    struct drm_client_dev client;
    struct drm_client_buffer *buffer;
    struct iosys_map map;
    int mapped;
    struct nmx_fb_info info;
};

/* one bridge per PCI function the port drives */
#define MAX_FB 8
static struct { void *dev; struct nmx_fb *fb; } fbs[MAX_FB];

/*
 * amdgpu may put scan-out buffers in system memory on APUs (scatter-gather display); the bridge needs them in
 * contiguous VRAM so one physical range describes the whole frame. Set before the driver initialises its display.
 */
extern int amdgpu_sg_display;

void nmx_fb_before_attach(void)
{
    amdgpu_sg_display = 0;
}

/* the DRM device of the driver attached to the PCI function 'dev' (amdgpu keeps it as the PCI driver data) */
static struct drm_device *drm_of(device_t dev)
{
    device_t *kids = NULL;
    int n = 0;
    struct drm_device *drm = NULL;
    if (device_get_children(dev, &kids, &n) != 0)
        return NULL;
    for (int i = 0; i < n && drm == NULL; i++) {
        if (!device_is_attached(kids[i]))
            continue;
        struct pci_dev *pdev = device_get_softc(kids[i]);
        if (pdev != NULL)
            drm = pci_get_drvdata(pdev);
    }
    free(kids, M_TEMP);
    return drm;
}

/* a monitor plugged or unplugged: probe again and light what is there now with the same buffer */
static int bridge_hotplug(struct drm_client_dev *client)
{
    struct nmx_fb *fb = container_of(client, struct nmx_fb, client);
    struct drm_mode_set *ms;
    if (fb->buffer == NULL)
        return 0;
    if (drm_client_modeset_probe(client, fb->info.width, fb->info.height) != 0)
        return 0;
    mutex_lock(&client->modeset_mutex);
    drm_client_for_each_modeset(ms, client)
        if (ms->mode != NULL)
            ms->fb = fb->buffer->fb;
    mutex_unlock(&client->modeset_mutex);
    drm_client_modeset_commit(client);
    return 0;
}

static void bridge_free(struct nmx_fb *fb);

/* a userland KMS client (a DRM master) closed: put our scan-out back on the outputs */
static int bridge_restore(struct drm_client_dev *client)
{
    return drm_client_modeset_commit(client);
}

/*
 * The DRM device is going away (driver detach): the DRM core has taken the client off its list and hands it back
 * here to free. Once registered, this is the only place a bridge is freed.
 */
static void bridge_unregister(struct drm_client_dev *client)
{
    struct nmx_fb *fb = container_of(client, struct nmx_fb, client);
    for (int i = 0; i < MAX_FB; i++)
        if (fbs[i].fb == fb) {
            fbs[i].dev = NULL;
            fbs[i].fb = NULL;
        }
    bridge_free(fb);
}

static const struct drm_client_funcs bridge_funcs = {
    .owner = NULL,
    .unregister = bridge_unregister,
    .restore = bridge_restore,
    .hotplug = bridge_hotplug,
};

static void bridge_free(struct nmx_fb *fb)
{
    if (fb->mapped)
        drm_client_buffer_vunmap(fb->buffer);
    if (fb->buffer != NULL)
        drm_client_framebuffer_delete(fb->buffer);
    drm_client_release(&fb->client);
    kfree(fb);
}

int nmx_fb_open(void *devp, struct nmx_fb_info *out)
{
    device_t dev = devp;
    struct drm_device *drm = drm_of(dev);
    struct drm_mode_set *ms;
    const struct drm_display_mode *best = NULL;
    int slot = -1, err;

    memset(out, 0, sizeof(*out));
    if (drm == NULL)
        return ENODEV;
    for (int i = 0; i < MAX_FB; i++)
        if (fbs[i].dev == NULL) { slot = i; break; }
    if (slot < 0)
        return ENOSPC;

    struct nmx_fb *fb = kzalloc(sizeof(*fb), GFP_KERNEL);
    if (fb == NULL)
        return ENOMEM;
    err = -drm_client_init(drm, &fb->client, "nmxfb", &bridge_funcs);
    if (err) {
        printf("NullMoth display: DRM client refused (%d)\n", err);
        kfree(fb);
        return err;
    }
    err = -drm_client_modeset_probe(&fb->client, 0, 0);
    if (err) {
        printf("NullMoth display: probing the outputs failed (%d)\n", err);
        goto fail;
    }
    /* the buffer is as large as the largest mode any connected output asked for */
    mutex_lock(&fb->client.modeset_mutex);
    drm_client_for_each_modeset(ms, &fb->client)
        if (ms->mode != NULL && (best == NULL ||
            (u64)ms->mode->hdisplay * ms->mode->vdisplay > (u64)best->hdisplay * best->vdisplay))
            best = ms->mode;
    if (best != NULL) {
        out->width = best->hdisplay;
        out->height = best->vdisplay;
        out->pclk_khz = (uint32_t)best->clock;
        out->htotal = best->htotal;
        out->vtotal = best->vtotal;
        out->hsync_start = best->hsync_start;
        out->hsync_end = best->hsync_end;
        out->vsync_start = best->vsync_start;
        out->vsync_end = best->vsync_end;
    }
    mutex_unlock(&fb->client.modeset_mutex);
    if (best == NULL) {
        printf("NullMoth display: no output is connected\n");
        err = ENXIO;
        goto fail;
    }

    fb->buffer = drm_client_framebuffer_create(&fb->client, out->width, out->height, DRM_FORMAT_XRGB8888);
    if (IS_ERR(fb->buffer)) {
        err = -PTR_ERR(fb->buffer);
        fb->buffer = NULL;
        printf("NullMoth display: no %ux%u scan-out buffer (%d)\n", out->width, out->height, err);
        goto fail;
    }
    out->pitch = fb->buffer->fb->pitches[0];

    err = -drm_client_buffer_vmap(fb->buffer, &fb->map);
    if (err) {
        printf("NullMoth display: the scan-out buffer would not map (%d)\n", err);
        goto fail;
    }
    fb->mapped = 1;

    /* the published range must be one physical run: check every page of the CPU view */
    const char *va = fb->map.is_iomem ? (const char *)fb->map.vaddr_iomem : (const char *)fb->map.vaddr;
    uint64_t size = (uint64_t)out->pitch * out->height;
    uint64_t base = nmx_xnu_kva_to_phys(va);
    for (uint64_t off = PAGE_SIZE; off < size; off += PAGE_SIZE)
        if (nmx_xnu_kva_to_phys(va + off) != base + off) {
            printf("NullMoth display: the scan-out buffer is not physically contiguous (page %llu)\n",
                   (unsigned long long)(off / PAGE_SIZE));
            err = EFAULT;
            goto fail;
        }
    if (base == 0) {
        err = EFAULT;
        goto fail;
    }
    out->phys = base;
    out->size = round_page(size);
    out->kva = (uint64_t)(uintptr_t)va;
    memset(__DECONST(char *, va), 0, size);       /* black until WindowServer draws */

    mutex_lock(&fb->client.modeset_mutex);
    drm_client_for_each_modeset(ms, &fb->client)
        if (ms->mode != NULL)
            ms->fb = fb->buffer->fb;
    mutex_unlock(&fb->client.modeset_mutex);
    err = -drm_client_modeset_commit(&fb->client);
    if (err) {
        printf("NullMoth display: the mode set failed (%d)\n", err);
        goto fail;
    }
    drm_client_register(&fb->client);       /* from here on hotplug reaches bridge_hotplug */

    fb->info = *out;
    fbs[slot].dev = dev;
    fbs[slot].fb = fb;
    out->ok = 1;
    printf("NullMoth display: %ux%u, pitch %u, phys 0x%llx, %u kHz pixel clock\n", out->width, out->height,
           out->pitch, (unsigned long long)out->phys, out->pclk_khz);
    return 0;

fail:
    bridge_free(fb);
    memset(out, 0, sizeof(*out));
    return err ? err : EIO;
}

/*
 * The framebuffer is gone from macOS: blank the outputs. The client itself is freed by the DRM core when the driver
 * detaches (bridge_unregister), which the kext does right after.
 */
void nmx_fb_close(void *dev)
{
    for (int i = 0; i < MAX_FB; i++)
        if (fbs[i].dev == dev && fbs[i].fb != NULL)
            drm_client_modeset_dpms(&fbs[i].fb->client, DRM_MODE_DPMS_OFF);
}

/* display sleep and wake from macOS (IOFramebuffer power) */
int nmx_fb_power(void *dev, int on)
{
    for (int i = 0; i < MAX_FB; i++)
        if (fbs[i].dev == dev)
            return -drm_client_modeset_dpms(&fbs[i].fb->client, on ? DRM_MODE_DPMS_ON : DRM_MODE_DPMS_OFF);
    return ENODEV;
}
