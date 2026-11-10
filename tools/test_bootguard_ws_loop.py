#!/usr/bin/env python3
"""The boot-guard job tells a desktop that stayed up from a WindowServer that keeps dying.

When WindowServer cannot get a Metal device for a display this driver drives, CoreDisplay asserts "Failed to create
MetalDevice" and launchd restarts WindowServer every ten seconds: a black screen with only the cursor. The head is lit,
so the kernel's guard was already cleared, and the job saw *a* WindowServer at both of its looks and called the start
good, so nothing ever turned the driver off. The job is lifted out of install.sh and run against fake pgrep, sysctl,
nvram, stat, sleep and shutdown. The negative control is the job as 2.8.1 shipped it (tag v2.8.1), which must call the looping
WindowServer good."""
import os
import pathlib
import stat
import subprocess
import sys
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
fails = 0


def arm(name, ok, detail=""):
    global fails
    fails += not ok
    print(("ok   " if ok else "FAIL ") + name + ("" if ok else f" ({detail.strip()[-400:]})"))


def job_from(src):
    start = src.index("cat > \"$BG.tmp\" <<'EOF' || die \"write the boot-guard job\"\n")
    body = src[src.index("\n", start) + 1:]
    return body[:body.index("\nEOF\n") + 1]


FAKES = {
    # one WindowServer pid per call, from $WS_PIDS (the last one repeats); none when the list is empty
    "pgrep": r'''#!/bin/bash
n=$(cat "$T/pgrep.n" 2>/dev/null || echo 0); echo $((n + 1)) > "$T/pgrep.n"
set -- $WS_PIDS; [ $# -gt 0 ] || exit 1
i=$((n + 1)); [ $i -le $# ] || i=$#
eval "echo \${$i}"''',
    "sysctl": r'''#!/bin/bash
case "$2" in
  kern.boottime) echo "{ sec = $BOOT_SEC, usec = 912475 } Sat Nov  7 22:00:00 2026";;
  debug.nvaccelfb) [ -n "$ARMED" ] && echo "$ARMED" || exit 1;;
  *) exit 1;;
esac''',
    "nvram": r'''#!/bin/bash
echo "nvram $*" >> "$T/calls"
case "$1" in
  -d) rm -f "$T/nv.${2##*:}";;
  *=*) k=${1%%=*}; echo "${1#*=}" > "$T/nv.${k##*:}";;
  *) [ -f "$T/nv.${1##*:}" ] || exit 1; printf '%s\t%s\n' "$1" "$(cat "$T/nv.${1##*:}")";;
esac''',
    # stat -f %Su /dev/console is the console user; anything else goes to the real stat
    "stat": r'''#!/bin/bash
if [ "$3" = /dev/console ]; then echo "$CONSOLE"; exit 0; fi
exec /usr/bin/stat "$@"''',
    # the 45 s wait: a WindowServer that dies meanwhile leaves its report
    "sleep": r'''#!/bin/bash
n=$(cat "$T/sleep.n" 2>/dev/null || echo 0); echo $((n + 1)) > "$T/sleep.n"
if [ "$1" = 45 ] && [ "$n" -lt "${CRASHES:-0}" ]; then : > "$DRDIR/WindowServer-2026-11-07-22$n.ips"; fi
exit 0''',
    "shutdown": r'''#!/bin/bash
echo "shutdown $*" >> "$T/calls"''',
}


def run(job, *, pids, armed="1", crashes=0, console="root", prior_loop=None):
    t = pathlib.Path(tempfile.mkdtemp(prefix="bgws-"))
    fake = t / "bin"
    fake.mkdir()
    for name, body in FAKES.items():
        p = fake / name
        p.write_text(body + "\n")
        p.chmod(p.stat().st_mode | stat.S_IXUSR)
    dr = t / "DiagnosticReports"
    dr.mkdir()
    # a report from an earlier boot must not count
    old = dr / "WindowServer-2026-11-01-000000.ips"
    old.write_text("")
    os.utime(old, (time.time() - 86400, time.time() - 86400))
    if prior_loop is not None:
        (t / "nv.nullmoth-ws-loop").write_text(prior_loop + "\n")
    text = job.replace("PATH=/usr/bin:/bin:/usr/sbin:/sbin", f"PATH={fake}:/usr/bin:/bin:/usr/sbin:/sbin")
    text = text.replace("LOG=/Library/NullMoth/bootguard.log", f"LOG={t}/bootguard.log")
    text = text.replace("DR=/Library/Logs/DiagnosticReports", f"DR={dr}")
    script = t / "bootguard.sh"
    script.write_text(text)
    env = dict(os.environ, T=str(t), WS_PIDS=" ".join(map(str, pids)), ARMED=armed or "", CRASHES=str(crashes),
               CONSOLE=console, DRDIR=str(dr), BOOT_SEC=str(int(time.time()) - 120))
    p = subprocess.run(["/bin/bash", str(script)], env=env, capture_output=True, text=True, timeout=120)
    log = (t / "bootguard.log").read_text() if (t / "bootguard.log").exists() else ""
    calls = (t / "calls").read_text() if (t / "calls").exists() else ""
    return p, log, calls


src = (ROOT / "package/install.sh").read_text()
job = job_from(src)
arm("the job still says when a start is good", "this start counts as good" in job)
arm("and reads WindowServer reports from this boot", "DiagnosticReports" in job and "kern.boottime" in job)

loop = list(range(500, 900, 10))   # a new WindowServer at every look

p, log, calls = run(job, pids=[777])
arm("a WindowServer that stays up for the wait counts as good", "counts as good" in log, log + p.stderr)
arm("  and clears the start count and sets nv-ok", "nvram -d 7C436110-AB2A-4BBB-A880-FE41995C9F82:nullmoth-nv-starts" in calls
    and "nullmoth-nv-ok=1" in calls, calls)
arm("  and turns nothing off", "nv-starts=%02" not in calls and "shutdown" not in calls, calls)

p, log, calls = run(job, pids=loop, crashes=99)
arm("a WindowServer that keeps dying on the NVIDIA display turns the driver off for the next boot",
    "nullmoth-nv-starts=%02" in calls and "nvram -d 7C436110-AB2A-4BBB-A880-FE41995C9F82:nullmoth-nv-ok" in calls, log + calls)
arm("  and never calls that start good", "counts as good" not in log and "nullmoth-nv-ok=1" not in calls, log)
arm("  and restarts once from the login window", "shutdown -r now" in calls, calls)
arm("  and leaves the reason for the desktop report", "nullmoth-ws-loop=" in calls, calls)

p, log, calls = run(job, pids=loop, crashes=99, console="user1")
arm("with someone logged in it turns the driver off but does not restart", "nv-starts=%02" in calls
    and "shutdown" not in calls, calls)

p, log, calls = run(job, pids=loop, crashes=99, prior_loop="5")
arm("a second loop in a row does not restart again", "nv-starts=%02" in calls and "shutdown" not in calls, calls)

p, log, calls = run(job, pids=loop, crashes=99, armed=None)
arm("a WindowServer loop on a display this driver does not drive is left alone", "nv-starts=%02" not in calls
    and "shutdown" not in calls and "four minutes" in log, log + calls)

p, log, calls = run(job, pids=[600, 610, 620, 620, 620, 620, 620, 620, 620, 620], crashes=2)
arm("two crashes at boot and then a steady WindowServer still counts as good", "counts as good" in log
    and "nv-starts=%02" not in calls, log + calls)

p, log, calls = run(job, pids=loop, crashes=0)
arm("a WindowServer seen as four processes counts as a loop even with no reports", "nv-starts=%02" in calls, log + calls)

# negative control: the job as 2.8.1 shipped it (tag v2.8.1) calls the looping WindowServer good
base = subprocess.run(["git", "-C", str(ROOT), "show", "v2.8.1:package/install.sh"], capture_output=True, text=True)
if base.returncode == 0:
    p, log, calls = run(job_from(base.stdout), pids=loop, crashes=99)
    arm("negative control: the 2.8.1 job calls a WindowServer that keeps dying good (the bug)",
        "counts as good" in log and "nv-starts=%02" not in calls, log + calls)
else:
    arm("negative control: 2.8.1's install.sh is readable", False, base.stderr)

print(f"{fails} failed")
sys.exit(1 if fails else 0)
