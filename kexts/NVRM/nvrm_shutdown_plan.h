/* What the driver puts back before the machine powers off or restarts. Pure functions, no kernel calls, so
 * tools/test_shutdown_plan.py can compile and exercise them on a host.
 *
 * placeLargeBar1 moves BAR1 and BAR3, can change the card's Resizable BAR size, and rewrites the parent root port's
 * prefetchable window. NVIDIA's Linux driver hands the device back through nv_pci_shutdown (RM shut down, bus mastering
 * cleared) and never leaves a layout of its own behind; IOPCIFamily restores config space on wake from sleep but does
 * nothing at power-off. A board whose firmware then reads a layout it did not make, with the card still mastering the
 * bus, is the shape of the AM5 no-POST reports. The plan below returns the firmware's own values, in the order the
 * placement's own rollback uses: memory decode and bus mastering off first, then the resize, the card's BARs, and the
 * root port window closed before it is reopened at its old range. */
#ifndef NVRM_SHUTDOWN_PLAN_H
#define NVRM_SHUTDOWN_PLAN_H

struct NvrmBarUndo {
    bool valid;                       // true once placeLargeBar1 moved something
    unsigned short cmd;               // the card's command word before the move
    bool resized; unsigned rbCtlOff, rbCtlOld;   // Resizable BAR control register and its old value
    unsigned lo14, hi18, lo1c, hi20;  // BAR1 low/high, BAR3 low/high as the firmware left them
    unsigned rp24, rp28, rp2c;        // root port prefetchable base/limit, base upper 32, limit upper 32
};

enum NvrmStepKind { kStepCmd16 = 1, kStepExt32 = 2, kStepCfg32 = 3, kStepRootPort32 = 4 };
struct NvrmStep { int kind; unsigned off; unsigned value; };

// Returns the number of steps written to out (0 when nothing was moved). Always ends with the card's command word at
// its old value with bus mastering off.
static inline unsigned nvrmBuildShutdownPlan(const NvrmBarUndo *u, NvrmStep *out, unsigned max)
{
    if (!u || !u->valid || max < 12) return 0;
    unsigned n = 0;
    out[n++] = { kStepCmd16, 0x04, (unsigned)(u->cmd & ~0x6u) };                 // memory decode and bus master off
    if (u->resized) out[n++] = { kStepExt32, u->rbCtlOff, u->rbCtlOld };       // size changes only while decode is off
    out[n++] = { kStepCfg32, 0x14, u->lo14 }; out[n++] = { kStepCfg32, 0x18, u->hi18 };
    out[n++] = { kStepCfg32, 0x1c, u->lo1c }; out[n++] = { kStepCfg32, 0x20, u->hi20 };
    out[n++] = { kStepRootPort32, 0x24, 0x0000fff0u };                          // close the window while its upper halves change
    out[n++] = { kStepRootPort32, 0x28, u->rp28 }; out[n++] = { kStepRootPort32, 0x2c, u->rp2c };
    out[n++] = { kStepRootPort32, 0x24, u->rp24 };
    out[n++] = { kStepCmd16, 0x04, (unsigned)(u->cmd & ~0x4u) };                 // the firmware's decode setting, no bus master
    return n;
}

// Two ways to bring the GPU down at power-off. Standby is the path system sleep exercises on every machine. Full is
// rm_shutdown_adapter, as Linux's nv_pci_shutdown ends up there: it frees the RM's adapter state and is only safe once no
// user client holds RM objects. It is not exercised anywhere on macOS before the unload path, so it runs only when the
// user asks for it (boot argument nvrmshutdown=full) and every user client has closed; otherwise, and whenever either
// condition fails, standby runs.
enum NvrmShutdownMode { kShutdownStandby = 0, kShutdownFull = 1 };
static inline NvrmShutdownMode nvrmShutdownMode(bool fullRequested, unsigned openUserClients, bool adapterUp)
{
    return (fullRequested && openUserClients == 0 && adapterUp) ? kShutdownFull : kShutdownStandby;
}

#endif
