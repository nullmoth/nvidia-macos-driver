/* FreeBSD <sys/pciio.h>: only the selector type LinuxKPI embeds. */
#pragma once
#include <stdint.h>
struct pcisel { uint32_t pc_domain; uint8_t pc_bus; uint8_t pc_dev; uint8_t pc_func; };
