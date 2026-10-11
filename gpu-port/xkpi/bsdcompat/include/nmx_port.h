/* The kext's interface to the port (implemented in src/nmx_port.c); plain types only, shared with the C++ side. */
#ifndef NMX_PORT_H
#define NMX_PORT_H
#ifdef __cplusplus
extern "C" {
#endif
int nmx_port_start(void);
void nmx_port_stop(void);
/* device-node opens still outstanding: the kext must not stop while this is nonzero */
int nmx_port_busy(void);
void *nmx_port_attach(void *iopci);
void nmx_port_detach(void *dev);
int nmx_port_power(void *dev, int on);
int nmx_port_shutdown(void *dev);
#ifdef __cplusplus
}
#endif
#endif
