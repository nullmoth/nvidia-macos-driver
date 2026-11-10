#!/usr/bin/env python3
"""The video codec library's diagnostic flag files count only when root made them, and what diagnostics write never
follows a link.

libnvencvt is loaded into every app that decodes or encodes video. It used to test /tmp/nvencvt.log, .stats, .dump,
.anyfmt, .refs, .dpb and .barcode with access() and fopen(): any local user could create one and switch on, in another
user's apps, a log, a statistics file, or a dump of the encoded stream and the first raw frames, written to a predictable
/tmp name with fopen (which follows a link someone else planted). vendor/nvenc/vt/nvencvt_diag.h holds the rule; it is
compiled and run here. The control is the library before the change (commit 406d9824)."""
import os, pathlib, shutil, stat, subprocess, sys, tempfile

R = pathlib.Path(__file__).resolve().parents[1]
VT = R / "vendor/nvenc/vt"
fails = 0


def arm(name, ok, detail=""):
    global fails
    print(("ok   " if ok else "FAIL ") + name + (f" ({str(detail).strip()[-200:]})" if detail and not ok else ""))
    fails += not ok


HARNESS = r'''
#include <string.h>
#include "nvencvt_diag.h"
int main(int argc, char **argv) {
    if (!strcmp(argv[1], "flag")) { printf("%d\n", nvencvt_flag(argv[2])); return 0; }
    if (!strcmp(argv[1], "out")) { FILE *f = nvencvt_out(argv[2], argv[3]); if (!f) { puts("refused"); return 0; } fputs("x", f); fclose(f); puts("written"); return 0; }
    return 2;
}
'''
cc = shutil.which("cc") or shutil.which("clang")
if not cc:
    print("SKIP: needs a C compiler"); sys.exit(2)
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d); (d / "h.c").write_text(HARNESS)
    subprocess.run([cc, "-Wall", "-Werror", "-I", str(VT), "-o", str(d / "h"), str(d / "h.c")], check=True)
    run = lambda *a: subprocess.run([str(d / "h"), *map(str, a)], capture_output=True, text=True).stdout.strip()  # noqa: E731
    mine = d / "nvencvt.log"; mine.write_text("")
    arm("a flag file an ordinary user made is not honoured", run("flag", mine) == "-1")
    arm("a missing flag file is not honoured", run("flag", d / "none") == "-1")
    rooted = next((p for p in ("/etc/hosts", "/etc/passwd") if os.path.isfile(p) and os.stat(p).st_uid == 0 and not os.stat(p).st_mode & 0o022), None)
    arm("control: a root-owned file nobody else can write is honoured", rooted is not None and run("flag", rooted) == "0", rooted)
    link = d / "flaglink"; link.symlink_to(rooted or "/etc/hosts")
    arm("a link to a root-owned file is not a flag", run("flag", link) == "-1")
    victim = d / "victim.txt"; victim.write_text("keep")
    planted = d / "nvencvt-123.log"; planted.symlink_to(victim)
    arm("an output name that is a planted link is refused and the target is untouched",
        run("out", planted, "a") == "refused" and victim.read_text() == "keep")
    out = d / "nvencvt-124.log"
    arm("an output file is created for its owner alone", run("out", out, "a") == "written" and stat.S_IMODE(out.stat().st_mode) == 0o600)
    run("out", out, "a")
    arm("append appends", out.read_text() == "xx")
    run("out", out, "wb")
    arm("a dump starts its file again", out.read_text() == "x")

src = (VT / "nvencvt.c").read_text() + (VT / "nvencvt_dec.c").read_text()
import re  # noqa: E402
arm("no flag file is tested with access() or opened with fopen() any more", not re.search(r'(access|fopen)\("/tmp/', src))
arm("no diagnostic output is opened with fopen()", "fopen(p, " not in src and 'fopen(sf ? sf' not in src)
arm("all eight flag tests go through the rule", src.count('nvencvt_flag("/tmp/') + src.count('nvencvt_flag_open("/tmp/') == 8,
    src.count('nvencvt_flag("/tmp/') + src.count('nvencvt_flag_open("/tmp/'))
old = subprocess.run(["git", "-C", str(R), "show", "406d9824:vendor/nvenc/vt/nvencvt.c"], capture_output=True, text=True)
if old.returncode == 0:
    arm("control: the library before this change tested /tmp flag files with access()", 'access("/tmp/nvencvt.dump", F_OK)' in old.stdout)
else:
    arm("control: the earlier library is readable", False, old.stderr)
print("FAILED" if fails else "PASS"); sys.exit(1 if fails else 0)
