/* Hybrid laptops and idle power-down (kexts/NVRM/nv_rtd3_policy.h), on the host.
 *   cc -Wall -I../kexts/NVRM test_hybrid_rtd3.c -o /tmp/t && /tmp/t     -> 0 failed
 *   cc -DOLD_RULES ...   (the counts before the hybrid change: no accelerator clients, no foreign heads)  -> failures */
#include <stdio.h>
#include "nv_rtd3_policy.h"
static int fails;
#define ARM(name, cond) do { if (cond) printf("ok   %s\n", name); else { printf("FAIL %s\n", name); fails++; } } while (0)
#ifdef OLD_RULES
#define HEADS(o, f, h) (o)
#define CLIENTS(r, a) (r)
#else
#define HEADS(o, f, h) nv_rtd3_heads_lit(o, f, h)
#define CLIENTS(r, a) nv_rtd3_clients_total(r, a)
#endif
static int may(int heads, int clients, unsigned idle) { struct nv_rtd3 s = {1, NV_RTD3_ON, 1}; return nv_rtd3_may_power_off(&s, 1, heads, clients, idle); }
int main(void)
{
    ARM("iGPU drives the desktop (switch off), no Metal app: the GPU powers off after 60 s", may(HEADS(0, 1, 0), CLIENTS(0, 0), 60) == 1);
    ARM("iGPU drives the desktop, a Metal app is attached to the accelerator: the GPU stays on", may(HEADS(0, 1, 0), CLIENTS(0, 1), 600) == 0);
    ARM("switch on (the GPU composites for the iGPU panel): never powers off, even with no app", may(HEADS(0, 1, 1), CLIENTS(0, 0), 600) == 0);
    ARM("switch on, level 2: same", may(HEADS(0, 1, 2), CLIENTS(0, 0), 600) == 0);
    ARM("its own display lit: stays on", may(HEADS(1, 0, 0), CLIENTS(0, 0), 600) == 0);
    ARM("a foreign panel with the switch off says nothing about this GPU", nv_rtd3_heads_lit(0, 3, 0) == 0);
    printf("%d failed\n", fails);
    return fails != 0;
}
