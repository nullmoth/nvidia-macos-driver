/* The kext's interface to the port (implemented in src/nmx_port.c); plain types only, shared with the C++ side. */
#ifndef NMX_PORT_H
#define NMX_PORT_H
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif
int nmx_port_start(void);
void nmx_port_stop(void);
/* device-node opens still outstanding: the kext must not stop while this is nonzero */
int nmx_port_busy(void);
void *nmx_port_attach(void *iopci);
void *nmx_port_attach_pci(void *iopci);
void nmx_port_detach(void *dev);
int nmx_port_power(void *dev, int on);
int nmx_port_shutdown(void *dev);

/* the display bridge (override/src/nmx_fb_bridge.c): one scan-out buffer on every lit output of the device */
struct nmx_fb_info {
    int ok;
    uint32_t width, height, pitch;      /* the buffer: XRGB8888, 'pitch' bytes per row */
    uint64_t phys, size;                /* its physical range (contiguous, through the GPU aperture) */
    uint64_t kva;                       /* its kernel mapping */
    uint32_t pclk_khz;                  /* timing of the largest output's mode */
    uint32_t htotal, vtotal, hsync_start, hsync_end, vsync_start, vsync_end;
};
/* before the driver attaches: settings the bridge needs from the driver (scan-out buffers in contiguous VRAM) */
void nmx_fb_before_attach(void);
int nmx_fb_open(void *dev, struct nmx_fb_info *out);
void nmx_fb_close(void *dev);
int nmx_fb_power(void *dev, int on);
#ifdef __cplusplus
}
#endif
#endif
