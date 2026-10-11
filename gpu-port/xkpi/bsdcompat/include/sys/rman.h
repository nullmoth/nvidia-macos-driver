/* FreeBSD rman(9) resources on xnu: a struct resource is a mapped BAR or an interrupt vector of the PCI device. */
#ifndef NMXKPI_SYS_RMAN_H
#define NMXKPI_SYS_RMAN_H
#include <sys/types.h>
#include <machine/bus.h>
struct resource;
rman_res_t rman_get_start(struct resource *r);
rman_res_t rman_get_end(struct resource *r);
rman_res_t rman_get_size(struct resource *r);
void *rman_get_virtual(struct resource *r);
bus_space_tag_t rman_get_bustag(struct resource *r);
bus_space_handle_t rman_get_bushandle(struct resource *r);
int rman_get_rid(struct resource *r);
u_int rman_get_flags(struct resource *r);
#endif
