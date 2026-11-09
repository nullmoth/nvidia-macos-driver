#!/usr/bin/env python3
"""Sorts kernel logs from machines with no Metal device into the causes the accelerator itself logs.
   python3 hy_nometal_bucket.py log1.txt [log2.txt ...]      one line per log, then counts
   python3 hy_nometal_bucket.py --selftest

A log is put in the FIRST bucket whose line it contains, in this order. The phrases are the accelerator's and NVRM's own
log lines (kexts/NVRM/accel/nvrm-accel.cpp, kexts/NVRM/NVRM.cpp); --selftest fails when the sources no longer write one."""
import sys, re
from pathlib import Path

BUCKETS = [
    ("not-started",       "boot-arg nvaccel=1 absent: NOT starting",
     "the accelerator never started: boot-arg nvaccel=1 is missing"),
    ("withheld-no-gpu",   "MetalPluginName stays withheld so WindowServer starts on the firmware framebuffer",
     "no NVIDIA display head and no other GPU with a Metal accelerator shows the desktop (panel on an iGPU macOS cannot accelerate)"),
    ("headless-restored", "MetalPluginName restored WITHOUT arming",
     "no NVIDIA head; apps have the NVIDIA device, WindowServer stays on the other GPU"),
    ("late-head",         "published AFTER the arm",
     "a framebuffer head published after arming, so it has no pipe until re-armed"),
    ("flip-off-small-bar","BAR1 not placed and below 512 MB",
     "small BAR1 window: copy path only"),
    ("armed",             "auto-go: display stack:",
     "armed normally (the cause is not in the accelerator's log; look at the plugin log)"),
]
SRC = ["kexts/NVRM/accel/nvrm-accel.cpp", "kexts/NVRM/NVRM.cpp"]

def classify(text):
    for name, phrase, _ in BUCKETS:
        if phrase in text:
            return name
    if "NVRM" in text and "NVAccel" not in text:
        return "accel-never-logged"
    return "no-nvidia-lines"

def selftest():
    root = Path(__file__).resolve().parents[1]
    src = "".join((root / f).read_text(encoding="utf-8") for f in SRC)
    bad = 0
    def arm(name, ok):
        nonlocal bad
        print(("ok   " if ok else "FAIL ") + name)
        bad += 0 if ok else 1
    for name, phrase, _ in BUCKETS:
        arm(f"the sources still write the {name} line", phrase.split(" -- ")[0].replace("%u", "") in src or phrase in src)
        arm(f"a log with only the {name} line lands in {name}", classify("kernel: (NVAccel) " + phrase) == name)
    arm("a log with an earlier bucket's line wins over a later one",
        classify(BUCKETS[0][1] + "\n" + BUCKETS[5][1]) == "not-started")
    arm("NVRM lines with no accelerator line mean the accelerator never logged", classify("NVRM-xnu: auto-go: go(2) -> 0x0") == "accel-never-logged")
    arm("a log with no NVIDIA lines is its own bucket", classify("kernel: other things") == "no-nvidia-lines")
    print(f"{bad} failed")
    return 1 if bad else 0

def main(argv):
    if argv[:1] == ["--selftest"]:
        return selftest()
    if not argv:
        print(__doc__); return 2
    counts = {}
    for f in argv:
        try:
            text = Path(f).read_text(errors="replace")
        except OSError as e:
            print(f"{f}: cannot read ({e})"); continue
        b = classify(text); counts[b] = counts.get(b, 0) + 1
        print(f"{b:20} {f}")
    print()
    for b, n in sorted(counts.items(), key=lambda kv: -kv[1]):
        why = next((w for nm, _, w in BUCKETS if nm == b), "")
        print(f"{n:5}  {b:20} {why}")
    return 0

if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
