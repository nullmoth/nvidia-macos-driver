#!/usr/bin/env python3
"""Internal panels that NVKMS can not dim, but whose DPCD offers brightness over AUX, get a brightness control.

The planner (nvrm_edp_backlight.h) is compiled against a model of the panel's DPCD registers and checked for the register state
it leaves; the call sites are checked in nvrm-fb.cpp and nvrm_ddc.cpp (DPCD 0x700..0x7ff only). The control is the old driver:
no NVKMS backlight meant no control at all. RM evidence: the only backlight controls in the public RM interface are
NV0073_CTRL_CMD_SPECIFIC_{GET,SET}_BACKLIGHT_BRIGHTNESS, answered by GSP firmware; where it says not supported, nothing in RM
drives the panel, so the DPCD route is the only one left on this stack."""
import pathlib, subprocess, tempfile
root = pathlib.Path(__file__).resolve().parents[1]
fb = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
rm = (root / 'kexts/NVRM/nvrm_ddc.cpp').read_text()
for need in ['PE_parse_boot_argn("nvfbedpbl"', 'me->fEdp ? nvrmEdpSet(', 'fBacklight->useEdpAux(']:
    assert need in fb, need
assert 'r->addr < 0x700 || r->addr >= 0x800 || r->addr + r->len > 0x800' in rm, 'the AUX pass-through must stay inside the eDP panel-control range'
fixture = r'''
#include <cassert>
#include <cstdio>
#include <cstring>
#include "nvrm_edp_backlight.h"
struct Dpcd { unsigned char r[0x800]; } panel;
static void apply(const EdpBacklightCaps &c, unsigned percent) {
    EdpWrite w[5]; unsigned n = edpBacklightPlan(c, panel.r[kEdpBacklightMode], panel.r[kEdpDisplayCtl], percent, w);
    for (unsigned i = 0; i < n; i++) panel.r[w[i].addr] = w[i].value;
}
int main() {
    memset(&panel, 0, sizeof panel);
    // a BOE-style panel: TCON adjust (0x701 bit0), AUX set (0x702 bit1), two-byte level, currently in PWM-pin mode, backlight off
    panel.r[kEdpGeneralCap1] = 0x01; panel.r[kEdpBacklightCap] = 0x06; panel.r[kEdpBacklightMode] = 0x00;
    EdpBacklightCaps c = edpBacklightCaps(panel.r[kEdpGeneralCap1], panel.r[kEdpBacklightCap]);
    assert(c.auxSet && c.twoBytes);
    apply(c, 50);
    assert((panel.r[kEdpBacklightMode] & 3) == 2);                           // switched to DPCD control
    assert(panel.r[kEdpDisplayCtl] & 1);                                     // backlight enabled
    assert(((panel.r[kEdpBacklightMsb] << 8) | panel.r[kEdpBacklightLsb]) == 32768);   // 50% of 0xFFFF
    apply(c, 100); assert(panel.r[kEdpBacklightMsb] == 0xff && panel.r[kEdpBacklightLsb] == 0xff);
    apply(c, 0);   assert(panel.r[kEdpBacklightMsb] == 0 && panel.r[kEdpBacklightLsb] == 0);
    apply(c, 250); assert(panel.r[kEdpBacklightMsb] == 0xff);                // clamped
    // already configured: a level change writes the level alone (no needless mode or enable writes)
    EdpWrite w[5]; unsigned n = edpBacklightPlan(c, panel.r[kEdpBacklightMode], panel.r[kEdpDisplayCtl], 30, w);
    assert(n == 2 && w[0].addr == kEdpBacklightMsb && w[1].addr == kEdpBacklightLsb);
    // other bits of the mode and control registers are preserved
    memset(&panel, 0, sizeof panel); panel.r[kEdpBacklightMode] = 0xf1; panel.r[kEdpDisplayCtl] = 0x80;
    apply(c, 10); assert(panel.r[kEdpBacklightMode] == 0xf2 && panel.r[kEdpDisplayCtl] == 0x81);
    // one-byte sink
    memset(&panel, 0, sizeof panel); EdpBacklightCaps one = edpBacklightCaps(0x01, 0x02); assert(one.auxSet && !one.twoBytes);
    apply(one, 50); assert(panel.r[kEdpBacklightMsb] == 128 && panel.r[kEdpBacklightLsb] == 0);
    // a PWM-pin-only panel (0x702 bit 0, no bit 1) or a panel without TCON adjust is never written
    memset(&panel, 0, sizeof panel); EdpBacklightCaps pwm = edpBacklightCaps(0x01, 0x01); assert(!pwm.auxSet);
    apply(pwm, 50); for (unsigned i = 0; i < 0x800; i++) assert(panel.r[i] == 0);
    EdpBacklightCaps notcon = edpBacklightCaps(0x00, 0x06); assert(!notcon.auxSet);
    puts("PASS: DPCD brightness plan: mode, enable and level written once, other bits kept, PWM-only panels untouched");
}
'''
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d); (d/'t.cpp').write_text(fixture)
    subprocess.run(['clang++','-std=c++17','-Wall','-Werror',f'-I{root}/kexts/NVRMFB',str(d/'t.cpp'),'-o',str(d/'t')],check=True)
    subprocess.run([str(d/'t')],check=True)
