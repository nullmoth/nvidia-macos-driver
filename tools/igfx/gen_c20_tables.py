#!/usr/bin/env python3
"""Extract the C20 PHY PLL tables from Linux's i915 display code (MIT licence) into kexts/NVRM/accel/nm_hybrid_c20tables.h.

usage: gen_c20_tables.py <path to intel_cx0_phy.c> > nm_hybrid_c20tables.h
The tables are copied value for value (not generated), so the header can be regenerated and diffed against a newer Linux.
"""
import re, sys

src = open(sys.argv[1]).read()
WANT = ["mtl_c20_dp_rbr", "mtl_c20_dp_hbr1", "mtl_c20_dp_hbr2", "mtl_c20_dp_hbr3", "mtl_c20_dp_uhbr10", "mtl_c20_dp_uhbr13_5", "mtl_c20_dp_uhbr20",
        "xe2hpd_c20_edp_r216", "xe2hpd_c20_edp_r243", "xe2hpd_c20_edp_r324", "xe2hpd_c20_edp_r432", "xe2hpd_c20_edp_r675", "xe2hpd_c20_dp_uhbr13_5"]
LISTS = ["mtl_c20_dp_tables", "xe2hpd_c20_edp_tables", "xe2hpd_c20_dp_tables", "xe3lpd_c20_dp_edp_tables"]
SEL = {"mtl_c20_dp_tables": "nmc20_tab_mtl_dp", "xe2hpd_c20_edp_tables": "nmc20_tab_xe2hpd_edp", "xe2hpd_c20_dp_tables": "nmc20_tab_xe2hpd_dp",
       "xe3lpd_c20_dp_edp_tables": "nmc20_tab_xe3lpd"}

def block(name):
    m = re.search(r"static const struct intel_c20pll_state %s = \{(.*?)\n\};" % re.escape(name), src, re.S)
    assert m, name
    body = m.group(1)
    clock = int(re.search(r"\.clock = (\d+)", body).group(1))
    def arr(field):
        mm = re.search(r"\.%s = \s*\{(.*?)\}" % field, body, re.S)
        if not mm: return None
        txt = re.sub(r"/\*.*?\*/", "", mm.group(1), flags=re.S)
        return [int(x, 16) for x in re.findall(r"0x[0-9a-fA-F]+", txt)]
    tx, cmn = arr("tx"), arr("cmn")
    mpllb, mplla = arr("mpllb"), arr("mplla")
    assert len(tx) == 3 and len(cmn) == 4
    assert (mpllb is None) != (mplla is None)
    return clock, tx, cmn, mpllb, mplla

def tablelist(name):
    m = re.search(r"static const struct intel_c20pll_state \* const %s\[\] = \{(.*?)\};" % re.escape(name), src, re.S)
    assert m, name
    return re.findall(r"&(\w+),", m.group(1))

out = []
out.append("""/* C20 PHY PLL tables: the register values of Linux's i915 display driver (drivers/gpu/drm/i915/display/intel_cx0_phy.c, MIT licence,
 * Copyright Intel Corporation), copied value for value by tools/igfx/gen_c20_tables.py. Do not edit by hand. */
#ifndef NM_HYBRID_C20TABLES_H
#define NM_HYBRID_C20TABLES_H
#include <stdint.h>

/* One PLL state. tx[0] bit 7 selects MPLLB (mp holds 11 words), else MPLLA (10 words). clock is the DisplayPort link rate in kHz of the
 * port clock (162000 = 1.62 Gb/s per lane). */
struct nmc20_state { uint32_t clock; uint16_t tx[3]; uint16_t cmn[4]; uint16_t mp[11]; };
""")
for n in WANT:
    clock, tx, cmn, b, a = block(n)
    mp = b if b is not None else a
    mp = mp + [0] * (11 - len(mp))
    out.append("static const struct nmc20_state nmc20_%s = { %d, { %s }, { %s }, { %s } };" % (n, clock, ", ".join("0x%04x" % x for x in tx), ", ".join("0x%04x" % x for x in cmn), ", ".join("0x%04x" % x for x in mp)))
out.append("")
for l in LISTS:
    names = tablelist(l)
    out.append("static const struct nmc20_state *const %s[] = { %s, 0 };" % (SEL[l], ", ".join("&nmc20_" + n for n in names)))
out.append("\n#endif")
print("\n".join(out))
