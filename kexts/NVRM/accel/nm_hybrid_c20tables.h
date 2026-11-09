/* C20 PHY PLL tables: the register values of Linux's i915 display driver (drivers/gpu/drm/i915/display/intel_cx0_phy.c, MIT licence,
 * Copyright Intel Corporation), copied value for value by tools/igfx/gen_c20_tables.py. Do not edit by hand. */
#ifndef NM_HYBRID_C20TABLES_H
#define NM_HYBRID_C20TABLES_H
#include <stdint.h>

/* One PLL state. tx[0] bit 7 selects MPLLB (mp holds 11 words), else MPLLA (10 words). clock is the DisplayPort link rate in kHz of the
 * port clock (162000 = 1.62 Gb/s per lane). */
struct nmc20_state { uint32_t clock; uint16_t tx[3]; uint16_t cmn[4]; uint16_t mp[11]; };

static const struct nmc20_state nmc20_mtl_c20_dp_rbr = { 162000, { 0xbe88, 0x5800, 0x0000 }, { 0x0500, 0x0005, 0x0000, 0x0000 }, { 0x50a8, 0x2120, 0xcd9a, 0xbfc1, 0x5ab8, 0x4c34, 0x2000, 0x0001, 0x6000, 0x0000, 0x0000 } };
static const struct nmc20_state nmc20_mtl_c20_dp_hbr1 = { 270000, { 0xbe88, 0x4800, 0x0000 }, { 0x0500, 0x0005, 0x0000, 0x0000 }, { 0x308c, 0x2110, 0xcc9c, 0xbfc1, 0x4b9a, 0x3f81, 0x2000, 0x0001, 0x5000, 0x0000, 0x0000 } };
static const struct nmc20_state nmc20_mtl_c20_dp_hbr2 = { 540000, { 0xbe88, 0x4800, 0x0000 }, { 0x0500, 0x0005, 0x0000, 0x0000 }, { 0x108c, 0x2108, 0xcc9c, 0xbfc1, 0x4b9a, 0x3f81, 0x2000, 0x0001, 0x5000, 0x0000, 0x0000 } };
static const struct nmc20_state nmc20_mtl_c20_dp_hbr3 = { 810000, { 0xbe88, 0x4800, 0x0000 }, { 0x0500, 0x0005, 0x0000, 0x0000 }, { 0x10d2, 0x2108, 0x8d98, 0xbfc1, 0x7166, 0x5f42, 0x2000, 0x0001, 0x7800, 0x0000, 0x0000 } };
static const struct nmc20_state nmc20_mtl_c20_dp_uhbr10 = { 1000000, { 0xbe21, 0xe800, 0x0000 }, { 0x0700, 0x0005, 0x0000, 0x0000 }, { 0x3104, 0xd105, 0xc025, 0xc025, 0x8c00, 0x759a, 0x4000, 0x0003, 0x3555, 0x0001, 0x0000 } };
static const struct nmc20_state nmc20_mtl_c20_dp_uhbr13_5 = { 1350000, { 0xbea0, 0x4800, 0x0000 }, { 0x0500, 0x0005, 0x0000, 0x0000 }, { 0x015f, 0x2205, 0x1b17, 0xffc1, 0xe100, 0xbd00, 0x2000, 0x0001, 0x4800, 0x0000, 0x0000 } };
static const struct nmc20_state nmc20_mtl_c20_dp_uhbr20 = { 2000000, { 0xbe20, 0x4800, 0x0000 }, { 0x0500, 0x0005, 0x0000, 0x0000 }, { 0x3104, 0xd105, 0x9217, 0x9217, 0x8c00, 0x759a, 0x4000, 0x0003, 0x3555, 0x0001, 0x0000 } };
static const struct nmc20_state nmc20_xe2hpd_c20_edp_r216 = { 216000, { 0xbe88, 0x4800, 0x0000 }, { 0x0500, 0x0005, 0x0000, 0x0000 }, { 0x50e1, 0x2120, 0x8e18, 0xbfc1, 0x9000, 0x78f6, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000 } };
static const struct nmc20_state nmc20_xe2hpd_c20_edp_r243 = { 243000, { 0xbe88, 0x4800, 0x0000 }, { 0x0500, 0x0005, 0x0000, 0x0000 }, { 0x50fd, 0x2120, 0x8f18, 0xbfc1, 0xa200, 0x8814, 0x2000, 0x0001, 0x1000, 0x0000, 0x0000 } };
static const struct nmc20_state nmc20_xe2hpd_c20_edp_r324 = { 324000, { 0xbe88, 0x4800, 0x0000 }, { 0x0500, 0x0005, 0x0000, 0x0000 }, { 0x30a8, 0x2110, 0xcd9a, 0xbfc1, 0x6c00, 0x5ab8, 0x2000, 0x0001, 0x6000, 0x0000, 0x0000 } };
static const struct nmc20_state nmc20_xe2hpd_c20_edp_r432 = { 432000, { 0xbe88, 0x4800, 0x0000 }, { 0x0500, 0x0005, 0x0000, 0x0000 }, { 0x30e1, 0x2110, 0x8e18, 0xbfc1, 0x9000, 0x78f6, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000 } };
static const struct nmc20_state nmc20_xe2hpd_c20_edp_r675 = { 675000, { 0xbe88, 0x4800, 0x0000 }, { 0x0500, 0x0005, 0x0000, 0x0000 }, { 0x10af, 0x2108, 0xce1a, 0xbfc1, 0x7080, 0x5e80, 0x2000, 0x0001, 0x6400, 0x0000, 0x0000 } };
static const struct nmc20_state nmc20_xe2hpd_c20_dp_uhbr13_5 = { 1350000, { 0xbea0, 0x4800, 0x0000 }, { 0x0500, 0x0005, 0x0000, 0x0000 }, { 0x015f, 0x2205, 0x1b17, 0xffc1, 0xbd00, 0x9ec3, 0x2000, 0x0001, 0x4800, 0x0000, 0x0000 } };

static const struct nmc20_state *const nmc20_tab_mtl_dp[] = { &nmc20_mtl_c20_dp_rbr, &nmc20_mtl_c20_dp_hbr1, &nmc20_mtl_c20_dp_hbr2, &nmc20_mtl_c20_dp_hbr3, &nmc20_mtl_c20_dp_uhbr10, &nmc20_mtl_c20_dp_uhbr13_5, &nmc20_mtl_c20_dp_uhbr20, 0 };
static const struct nmc20_state *const nmc20_tab_xe2hpd_edp[] = { &nmc20_mtl_c20_dp_rbr, &nmc20_xe2hpd_c20_edp_r216, &nmc20_xe2hpd_c20_edp_r243, &nmc20_mtl_c20_dp_hbr1, &nmc20_xe2hpd_c20_edp_r324, &nmc20_xe2hpd_c20_edp_r432, &nmc20_mtl_c20_dp_hbr2, &nmc20_xe2hpd_c20_edp_r675, &nmc20_mtl_c20_dp_hbr3, 0 };
static const struct nmc20_state *const nmc20_tab_xe2hpd_dp[] = { &nmc20_mtl_c20_dp_rbr, &nmc20_mtl_c20_dp_hbr1, &nmc20_mtl_c20_dp_hbr2, &nmc20_mtl_c20_dp_hbr3, &nmc20_mtl_c20_dp_uhbr10, &nmc20_xe2hpd_c20_dp_uhbr13_5, 0 };
static const struct nmc20_state *const nmc20_tab_xe3lpd[] = { &nmc20_mtl_c20_dp_rbr, &nmc20_xe2hpd_c20_edp_r216, &nmc20_xe2hpd_c20_edp_r243, &nmc20_mtl_c20_dp_hbr1, &nmc20_xe2hpd_c20_edp_r324, &nmc20_xe2hpd_c20_edp_r432, &nmc20_mtl_c20_dp_hbr2, &nmc20_xe2hpd_c20_edp_r675, &nmc20_mtl_c20_dp_hbr3, &nmc20_mtl_c20_dp_uhbr10, &nmc20_xe2hpd_c20_dp_uhbr13_5, &nmc20_mtl_c20_dp_uhbr20, 0 };

#endif
