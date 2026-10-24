#ifndef NMHDA_TOPOLOGY_H
#define NMHDA_TOPOLOGY_H
#include <stdint.h>
enum { NMHDA_TOPOLOGY_VERSION = 1, NMHDA_MAX_PINS = 64, NMHDA_ELD_BYTES = 256 };
struct NMHDAPin {
    uint8_t codec, function, pin, reserved;
    uint32_t sense, eldSize;
    uint8_t eld[NMHDA_ELD_BYTES];
};
struct NMHDATopology {
    uint32_t version, count;
    uint64_t generation;
    uint32_t unavailable, truncated;
    NMHDAPin pins[NMHDA_MAX_PINS];
};
#define NMHDA_TOPOLOGY_SERVICE "NMHDATopologyService"
#define NMHDA_TOPOLOGY_READ "NMHDAReadTopology"
#endif
