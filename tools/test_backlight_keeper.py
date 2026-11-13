#!/usr/bin/env python3
"""Panel brightness survives a restart on firmware that drops NVRAM writes (issue 582).

The driver saves the level in the NVRAM variable nullmoth-backlight. With OpenCore's DisableVariableWrite and no
emulated NVRAM the variable comes back holding an old value at every restart. The installed backlight job keeps a copy
on disk and gives it back through debug.nvrmfb_backlight_restore only when the variable disagrees with that copy.

Runs the job script from package/install.sh against stub nvram/sysctl commands, compiles the kernel's restore rule from
kexts/NVRMFB/nvrm_off.h, and reads the kernel wiring.
   python3 tools/test_backlight_keeper.py [tree-root]
Negative controls: a job with the disagreement check removed must restore on a working machine, and the kernel rule
with the user-moved check removed must accept a restore after the user moved the brightness; both must be caught."""
import base64
import os
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parents[1]
INSTALL = (ROOT / "package/install.sh").read_text(encoding="utf-8")
UNINSTALL = (ROOT / "package/uninstall.sh").read_text(encoding="utf-8")
HEADER = (ROOT / "kexts/NVRMFB/nvrm_off.h").read_text(encoding="utf-8")
FB = (ROOT / "kexts/NVRMFB/fb/nvrm-fb.cpp").read_text(encoding="utf-8")
GUID = "7C436110-AB2A-4BBB-A880-FE41995C9F82"
fails = []


def arm(name, ok, detail=""):
    print(("ok   " if ok else "FAIL ") + name + (f"  [{detail}]" if detail and not ok else ""))
    if not ok:
        fails.append(name)


def job_script():
    start = INSTALL.index('cat > "$BL.tmp" <<\'EOF\'')
    body = INSTALL[INSTALL.index("\n", start) + 1:]
    return body[:body.index("\nEOF\n")] + "\n"


def run_job(script, nvram_hex, saved_hex, sysctl_rc=0, panel=True):
    """Run the job once: start, let it reach its copy loop, send TERM. Returns (restores, saved file, log)."""
    with tempfile.TemporaryDirectory() as d:
        d = Path(d)
        stub = d / "bin"; stub.mkdir()
        nv = d / "nvram.plist"
        entry = (f"<key>{GUID}:nullmoth-backlight</key><data>{base64.b64encode(bytes.fromhex(nvram_hex)).decode()}</data>"
                 if nvram_hex else "")
        nv.write_text('<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>'
                      f'<key>boot-args</key><string>-v</string>{entry}</dict></plist>')
        (stub / "nvram").write_text(f'#!/bin/bash\n[ "$1" = -x ] && [ "$2" = -p ] && cat "{nv}"\n')
        (stub / "sysctl").write_text(
            "#!/bin/bash\n"
            f'if [ "$1" = -n ]; then {"echo -1" if panel else "exit 1"}; exit 0; fi\n'
            f'if [ "$1" = -w ]; then echo "$2" >> "{d}/restores"; exit {sysctl_rc}; fi\nexit 1\n')
        for f in ("nvram", "sysctl"):
            os.chmod(stub / f, 0o755)
        level, log = d / "backlight.level", d / "backlight.log"
        if saved_hex:
            level.write_text(saved_hex + "\n")
        s = script.replace("PATH=/usr/bin:/bin:/usr/sbin:/sbin", f"PATH={stub}:/usr/bin:/bin:/usr/sbin:/sbin")
        s = s.replace("F=/Library/NullMoth/backlight.level", f"F={level}").replace("LOG=/Library/NullMoth/backlight.log", f"LOG={log}")
        if not panel:
            s = s.replace("sleep 2\n", "sleep 0\n")
        js = d / "job.sh"; js.write_text(s)
        p = subprocess.Popen(["/bin/bash", str(js)])
        time.sleep(1.5)
        if p.poll() is None:
            p.send_signal(signal.SIGTERM)
        try:
            p.wait(timeout=10)
        except subprocess.TimeoutExpired:
            p.kill(); p.wait()
            return None, None, "job did not exit on TERM"
        restores = (d / "restores").read_text().split() if (d / "restores").exists() else []
        return restores, (level.read_text().strip() if level.exists() else None), (log.read_text() if log.exists() else "")


script = job_script()
OID = "debug.nvrmfb_backlight_restore"

# 1. The case in the report: NVRAM came back as 02 ff ff, the copy on disk says 02 00 40 (logical 0x4000).
r, saved, log = run_job(script, "02ffff", "020040")
arm("NVRAM came back with an old level: the saved copy is restored (logical 16384)", r == [f"{OID}=16384"], f"{r}")
arm("and the restore is logged with both values", "NVRAM held 02ffff, the saved copy 020040: level 16384 restored" in (log or ""), log)

# 2. A machine whose NVRAM works: the variable and the copy agree, the job must not touch the driver.
r, saved, _ = run_job(script, "028f40", "028f40")
arm("NVRAM kept the level (variable equals the copy): nothing is restored", r == [], f"{r}")

# 3. First start after the install: no copy yet. Nothing restored; the variable is copied at shutdown.
r, saved, _ = run_job(script, "028000", None)
arm("no copy on disk yet: nothing restored", r == [], f"{r}")
arm("and at TERM (shutdown) the current variable is copied to disk", saved == "028000", f"{saved}")

# 4. The driver refuses (the user already moved the slider): logged, not retried.
r, saved, log = run_job(script, "02ffff", "020040", sysctl_rc=1)
arm("a refused restore is logged as refused", "the driver refused the restore" in (log or ""), log)

# 5. No internal panel (the entry point never appears): the job ends without restoring or writing anything.
r, saved, _ = run_job(script, "02ffff", "020040", panel=False)
arm("no panel brightness control: no restore and the copy is left as it was", r == [] and saved == "020040", f"{r} {saved}")

# 6. A malformed copy on disk is ignored rather than turned into a level.
r, saved, _ = run_job(script, "02ffff", "zz0040")
arm("a malformed copy on disk is never restored", r == [], f"{r}")

# Negative control for the job: without the disagreement check a working machine would be restored every boot.
bad = script.replace('[ -n "$saved" ] && [ "$now" != "$saved" ]', '[ -n "$saved" ]')
arm("negative control: the check above is the one that keeps working machines untouched (removing it is caught)",
    bad != script and run_job(bad, "028f40", "028f40")[0] == [f"{OID}=16527"])

arm("the job is removed by the uninstaller with its copy and log",
    "com.nullmoth.backlight.plist /Library/NullMoth/backlight.sh /Library/NullMoth/backlight.level /Library/NullMoth/backlight.log" in UNINSTALL)


# Kernel rule: compile nvrm_backlight_external_restore_ok from the header.
def rule_src(header):
    a = header.index("static inline bool nvrm_backlight_external_restore_ok(")
    return header[a:header.index("\n}\n", a) + 3]


def run_rule(fn):
    prog = ("#include <cassert>\n#include <cstdint>\n#include <cstdio>\nusing SInt32=int32_t;\n"
            "enum { kNVRMBacklightMax = 0xFFFF };\n" + fn +
            "int main(){ int bits=0; bits|=nvrm_backlight_external_restore_ok(false,16384)<<0;"
            " bits|=nvrm_backlight_external_restore_ok(true,16384)<<1; bits|=nvrm_backlight_external_restore_ok(false,-1)<<2;"
            " bits|=nvrm_backlight_external_restore_ok(false,65536)<<3; bits|=nvrm_backlight_external_restore_ok(false,0)<<4;"
            " bits|=nvrm_backlight_external_restore_ok(false,65535)<<5; printf(\"%d\",bits); }\n")
    with tempfile.TemporaryDirectory() as d:
        c = Path(d) / "r.cpp"; c.write_text(prog)
        subprocess.run(["clang++", "-std=c++17", str(c), "-o", str(Path(d) / "r")], check=True)
        return int(subprocess.run([str(Path(d) / "r")], capture_output=True, text=True, check=True).stdout)


bits = run_rule(rule_src(HEADER))
arm("kernel: a restore before the user moved the brightness is accepted", bits & 1 == 1)
arm("kernel: a restore after the user moved it is refused", bits & 2 == 0)
arm("kernel: levels outside 0..65535 are refused, 0 and 65535 accepted", bits & 4 == 0 and bits & 8 == 0 and bits & 16 and bits & 32)
broken = rule_src(HEADER).replace("!userMoved && ", "")
arm("negative control: the rule without the user-moved check is caught (accepts after a move)",
    broken != rule_src(HEADER) and run_rule(broken) & 2 == 2)

doint = FB[FB.index("bool NVRMBacklightHandler::doIntegerSet("):]
doint = doint[:doint.index("\n}\n")]
arm("kernel: only a user request below the maximum counts as moving the brightness (the start-up maximum does not)",
    "if (level < (SInt32)kNVRMBacklightMax) fUserMoved = true;" in doint)
arm("kernel: the entry point is root-only (no CTLFLAG_ANYBODY)",
    "nvrmfb_backlight_restore, CTLTYPE_INT | CTLFLAG_RW | CTLFLAG_LOCKED" in FB)
stop = FB[FB.index("if (fBacklight) { nvrmBacklightClearGlobal(fBacklight);"):][:120]
arm("kernel: stop() takes the handler out of the global before releasing it", "fBacklight->detach(this); OSSafeReleaseNULL(fBacklight);" in stop)
arm("kernel: the published handler is retained under the lock by the sysctl before use",
    "if (h) h->retain();\n    IOLockUnlock(gBacklightLock);\n    if (!h) return ENODEV;" in FB)

print(f"{len(fails)} failed")
sys.exit(1 if fails else 0)
