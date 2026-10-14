#!/usr/bin/env python3
"""A Vulkan program on a healthy GPU prints nothing from the NVRM backend unless NVK_DIAG=1 asks for it.

A user's fastfetch printed seven "nvrm: ..." lines and a Mesa warning on every run and asked whether something was
broken; the lines were start-up notes, not errors. They now go out only under NVK_DIAG (which also sends stderr to
/private/tmp/nvk-diag-<pid>.log). Also checks that every hunk in the patch still counts its own lines."""
import pathlib, re, sys
patch = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1] / "nvk/nvk-macos.patch"
s = patch.read_text()
lines = s.split("\n")
fails = []

# Hunk bodies must match their headers, or the patch no longer applies.
i = 0
while i < len(lines):
    m = re.match(r"@@ -\d+(?:,(\d+))? \+\d+(?:,(\d+))? @@", lines[i])
    if not m:
        i += 1
        continue
    old, new = int(m.group(1) or 1), int(m.group(2) or 1)
    i += 1
    while old > 0 or new > 0:
        if i >= len(lines):
            fails.append("patch ends inside a hunk"); break
        t = lines[i][:1]
        if t == " ": old -= 1; new -= 1
        elif t == "-": old -= 1
        elif t == "+": new -= 1
        elif t == "\\": pass
        else:
            fails.append(f"hunk before patch line {i + 1} is short (old {old}, new {new} left)"); break
        i += 1

# Each start-up note, and the line its print sits on (the print may span lines; the gate is on the first).
NOTES = ["PERF_BOOST flags", "host-pointer import", "LOCAL+mapped memory", "copy-engine queue could not be built",
         "event wait ON", "flush waits sleep", "host pointer IMPORTED"]
for note in NOTES:
    at = [k for k, l in enumerate(lines) if l.startswith("+") and note in l and "fprintf(stderr" in l]
    if len(at) != 1:
        fails.append(f"{note!r}: {len(at)} prints found, expected 1"); continue
    k = at[0]
    gate = lines[k] if "if (" in lines[k] else lines[k - 1]
    if "nvkmd_nvrm_diag_on()" not in gate:
        fails.append(f"{note!r} prints without NVK_DIAG (patch line {k + 1})")
lg = [k for k, l in enumerate(lines) if "SET_LG_SECTOR_PROMOTION 64B rc" in l]
if len(lg) != 1 or "nvkmd_nvrm_diag_on()" not in lines[lg[0] - 1]:
    fails.append("SET_LG_SECTOR_PROMOTION refusal (every Turing process) prints without NVK_DIAG")

tags = [k + 1 for k, l in enumerate(lines) if l.startswith("+") and re.search(r'"nvrm: b\d\d ', l)]
if tags:
    fails.append(f"internal build tags in printed text at patch lines {tags[:5]}")

for f in fails:
    print("FAIL", f)
print("PASS" if not fails else f"{len(fails)} failed")
sys.exit(1 if fails else 0)
