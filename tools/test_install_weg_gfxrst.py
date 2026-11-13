#!/usr/bin/env python3
"""With WhateverGreen loaded and no gfxrst in boot-args, the installer adds gfxrst=4 itself.

WhateverGreen copies the boot screen on the first framebuffer start from a console mapping it saved at boot. macOS can
free that mapping before the NVIDIA display opens, and the copy then faults (panic under IOFramebuffer::postOpen).
gfxrst=4 turns the copy off. The installer used to print a note only. The rule is run here from the installer's own
function; the uninstaller's removal is run on its own lines against a stand-in nvram."""
import pathlib, subprocess, sys, tempfile, os

R = pathlib.Path(__file__).resolve().parents[1]
ins = (R / "package/install.sh").read_text()
unins = (R / "package/uninstall.sh").read_text()
fails = 0


def arm(name, ok, detail=""):
    global fails
    print(("ok   " if ok else "FAIL ") + name + (f" ({detail})" if detail and not ok else ""))
    fails += not ok


i = ins.index("weg_gfxrst() {"); fn = ins[i:ins.index("\n}\n", i) + 3]


def rule(args):
    return subprocess.run(["/bin/bash", "-c", fn + '\nweg_gfxrst "$1"', "x", args], capture_output=True, text=True).stdout.strip()


arm("no gfxrst: it is appended and the rest is kept", rule("nvfb=1 nvaccel=1") == "nvfb=1 nvaccel=1 gfxrst=4")
arm("empty boot-args: gfxrst=4 alone", rule("") == "gfxrst=4")
arm("gfxrst=4 already there: nothing to write", rule("nvfb=1 gfxrst=4") == "")
arm("a gfxrst the user chose stays as it is", rule("gfxrst=1 nvfb=1") == "")
arm("control: an argument that only contains the letters is not taken for one", rule("xgfxrst=4") == "xgfxrst=4 gfxrst=4")
blk = ins[ins.index("if kmutil showloaded --list-only --bundle-identifier as.vit9696.WhateverGreen"):]
blk = blk[:blk.index("\nfi\n") + 4]
arm("it is done only when WhateverGreen is loaded", "weg_gfxrst \"$ba\"" in blk and 'nvram boot-args="$nb"' in blk)
arm("the addition is recorded only after the write worked",
    blk.index('if nvram boot-args="$nb"') < blk.index("added-boot-args"))
arm("the config.plist note is still printed", "OpenCore config.plist" in blk)

j = unins.index('if grep -qx "gfxrst=4" /Library/NullMoth/added-boot-args'); rm = unins[j:unins.index("\nfi\n", j) + 4]
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d); (d / "bin").mkdir()
    (d / "bin/nvram").write_text('#!/bin/bash\nS="%s/state"\ncase "$1" in boot-args) [ -f "$S" ] && printf "boot-args\\t%%s\\n" "$(cat "$S")";; '
                                 'boot-args=*) printf "%%s" "${1#boot-args=}" > "$S";; -d) rm -f "$S";; esac\n' % d)
    os.chmod(d / "bin/nvram", 0o755)

    def removal(state, recorded):
        (d / "state").write_text(state); rec = d / "rec"
        if recorded: rec.write_text("gfxrst=4\n")
        elif rec.exists(): rec.unlink()
        subprocess.run(["/bin/bash", "-c", rm.replace("/Library/NullMoth/added-boot-args", str(rec))],
                       env={"PATH": f"{d}/bin:/usr/bin:/bin"}, check=True)
        return (d / "state").read_text() if (d / "state").exists() else None, rec.exists()

    arm("uninstall takes out the gfxrst=4 it recorded and keeps the rest", removal("nvfb=1 gfxrst=4 nvaccel=1", True) == ("nvfb=1 nvaccel=1", False))
    arm("uninstall deletes boot-args when gfxrst=4 was all there was", removal("gfxrst=4", True) == (None, False))
    arm("control: with no record, a gfxrst=4 the user set is left alone", removal("nvfb=1 gfxrst=4", False) == ("nvfb=1 gfxrst=4", False))
print("FAILED" if fails else "all ok")
sys.exit(1 if fails else 0)
