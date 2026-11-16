/* tgp_format.h: the notebook power-budget lines of nvmtl-smi, from the kext's debug.nvrm_tgp_* sysctls (nvrm_power.cpp).
   Pure over the numbers it is given so test_tgp_format.c proves it without a driver. A missing sysctl is passed as
   `present` 0: opt-in and laptop detection are both required before this interface exists. */
#ifndef NVMTL_TGP_FORMAT_H
#define NVMTL_TGP_FORMAT_H
#include <stdio.h>

struct tgp_view { int present, enable, state, ac, last_status, applies; };

static inline void tgp_format(const struct tgp_view *v, FILE *out)
{
    if (!v->present) { fprintf(out, "tgp_mode inactive (requires nvrmtgp=1 and a detected laptop battery)\n"); return; }
    fprintf(out, "tgp_mode %s", v->state < 0 ? "not sent yet" : v->state ? "on" : "off");
    fprintf(out, " (follow power source %s, power source %s)\n", v->enable ? "yes" : "no (forced off)",
            v->ac < 0 ? "unknown" : v->ac ? "mains" : "battery");
    fprintf(out, "tgp_controls_sent %d last_status 0x%x\n", v->applies, (unsigned)v->last_status);
}
#endif
