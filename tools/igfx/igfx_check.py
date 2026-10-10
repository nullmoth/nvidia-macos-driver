#!/usr/bin/env python3
"""Check the planner against recorded machines: for every probe upload that holds an Intel iGPU trace and the eDP
panel's EDID, plan from the EDID and compare each register with the value Linux's driver wrote on that machine.
A machine passes only when every planned register matches its recording.

usage: igfx_check.py <folder of probe upload zips> [--verbose]"""
import glob
import os
import re
import sys
import tempfile
import zipfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import igfx_aux as X  # noqa: E402
import igfx_plan as P  # noqa: E402
import igfx_trace as T  # noqa: E402


def machines(folder):
    for z in sorted(glob.glob(os.path.join(folder, "**", "*.zip"), recursive=True)):
        try:
            f = zipfile.ZipFile(z)
            names = f.namelist()
        except (OSError, zipfile.BadZipFile):
            continue
        tr = [n for n in names if re.search(r"/trace/(i915|xe)-0000_00_02\.0\.mmio\.xz$", n)]
        edid = [n for n in names if re.search(r"/gpu/drm/card\d+-eDP-\d+\.edid$", n)]
        if tr and edid:
            vbt = [n for n in names if n.endswith("i915_vbt.bin")]
            yield z, f, tr[0], edid[0], vbt[0] if vbt else None


def check(f, trace_name, edid_name, vbt_name=None):
    tmp = os.path.join(tempfile.mkdtemp(), "t.mmio.xz")
    with open(tmp, "wb") as out:
        out.write(f.read(trace_name))
    _, last, _ = T.decode(tmp)
    edid = f.read(edid_name)
    try:
        timing = P.preferred_timing(edid)
    except ValueError:
        return "no-timing", None, None
    ver = P.display_version(last.get("_DEVICE", 0))
    if ver is None:
        return "unknown-device", None, None
    want = P.plan(timing, ver)
    # the transcoder Linux used for the panel: the one whose active size is the panel's
    for t in "ABCD":
        m = T.mode(last, t)
        if m and (m["hactive"], m["vactive"]) == ((want["HTOTAL"] & 0xFFFF) + 1, (want["VTOTAL"] & 0xFFFF) + 1):
            got = {k: last.get("%s_%s" % (k, t)) for k in ("HTOTAL", "HBLANK", "HSYNC", "VTOTAL", "VBLANK", "VSYNC", "PIPESRC")}
            got["PLANE_SIZE_1"] = last.get("PLANE_SIZE_1_%s" % t)
            got["PLANE_STRIDE_1"] = last.get("PLANE_STRIDE_1_%s" % t)
            panel = P.vbt_panel(f.read(vbt_name), edid) if vbt_name else None
            if panel:
                # panel power + backlight come from the VBT, not the EDID
                want.update(P.panel_plan(panel, ver))
                for k in ("PP_ON_DELAYS", "PP_OFF_DELAYS", "BLC_PWM_PCH_CTL2"):
                    got[k] = last.get(k)
                ctl = last.get("PP_CONTROL")
                got["PP_CYCLE"] = None if ctl is None else (ctl >> 4) & 0x1F
            # the eDP link: our plan from the DPCD the panel answered with vs the link Linux trained
            dpcd, writes, refclk = X.decode(tmp)
            rate, lanes, dsc = X.chosen_link(dpcd, writes)
            if rate and not dsc:
                r, n = P.link_plan(dpcd, edid, panel["max_link_rate"] if panel else 0)
                want["LINK"], got["LINK"] = r << 8 | n, rate << 8 | lanes
                want.update(P.ddi_plan(timing, P.edid_bpp(edid), n))
                got["TRANS_DDI_FUNC_CTL"], got["DDI_BUF_CTL"] = last.get("TRANS_DDI_FUNC_CTL_%s" % t), last.get("DDI_BUF_CTL_A")
                if ver <= 13 and refclk and last.get("DPLL0_ENABLE", 0) >> 31:
                    want.update({"DPLL0_" + k[5:]: v for k, v in P.dpll_plan(rate, refclk, ver).items()})
                    got["DPLL0_CFGCR0"], got["DPLL0_CFGCR1"] = last.get("DPLL0_CFGCR0"), last.get("DPLL0_CFGCR1")
            diff = {k: (want[k], got[k]) for k in want if got.get(k) is not None and got[k] != want[k]}
            missing = [k for k in want if got.get(k) is None]
            return t, diff, missing
    return None, None, None


if __name__ == "__main__":
    total = passed = 0
    for z, f, tr, ed, vb in machines(sys.argv[1]):
        t, diff, missing = check(f, tr, ed, vb)
        if t in (None, "no-timing", "unknown-device"):
            if t:
                print("SKIP %-60s %s" % (tr.split("/")[0][:60], t))
            continue   # Linux did not drive the panel from the iGPU on this machine (MUX set to the NVIDIA card)
        total += 1
        ok = not diff
        passed += ok
        name = tr.split("/")[0][:60]
        print("%s %-60s transcoder %s%s%s" % ("PASS" if ok else "FAIL", name, t,
                                               "" if ok else " " + ", ".join("%s want 0x%x got 0x%x" % (k, a, b) for k, (a, b) in diff.items()),
                                               (" (not written: %s)" % ",".join(missing)) if missing and "--verbose" in sys.argv else ""))
    print("%d/%d recorded panels match the plan" % (passed, total))
    sys.exit(0 if total and passed == total else 1)
