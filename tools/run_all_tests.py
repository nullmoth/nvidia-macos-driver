#!/usr/bin/env python3
"""run_all_tests.py [-j N] [name ...]: every host test in tools/, in one run, with one answer.

The tests are separate programs (Python scripts, C and C++ files with their build line in the header comment, shell
scripts), and nothing ran them all: seven of them had been failing unnoticed because the code they lift pieces from had
moved on. This runs each one, prints the failures with the end of their output, and exits 1 if any failed.

  Python   tools/test_*.py, run as they are.
  C, C++   tools/test_*.c and .cpp whose header names their build ("cc <flags> <file> -o /tmp/x && /tmp/x"): built into a
           temporary folder with those flags and run. A file without such a line is listed as not run.
  Shell    tools/test_*.sh that take no argument.

Not run here, and listed at the end with the reason: tests that need a built library or an outside source tree. They are
run at the build step, where those exist. Needs a Mac with the Xcode command line tools (the tests compile Objective-C
and x86_64 code). Run it from a full clone: negative controls read older revisions of the source."""
import concurrent.futures
import os
import re
import shlex
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TOOLS = ROOT / "tools"
# name -> why this runner cannot run it
NEEDS_INPUT = {
    "test_fcwarm.py": "needs a built libnvmtl_translate.dylib and a shader file (build step)",
    "test_gl_translation.py": "needs a built libnvmtl_translate.dylib (build step)",
    "test_resource_phi_fold.py": "needs a built libnvmtl_translate.dylib (build step)",
    "test_patch_chain.sh": "needs a pristine NVIDIA open-gpu-kernel-modules 620.70.06 tree (build_nvrm.sh runs it)",
}
BUILD_LINE = re.compile(r"(?:^|[\s*/])(cc|c\+\+|clang|clang\+\+)\s+(.*?)\s+&&\s+\S+")
TIMEOUT = 600


def c_command(path, out):
    """-> (argv, cwd) from the header's build line, or None. The line is written for the repository root when it names the
    file as tools/<file>, and for tools/ otherwise."""
    head = "\n".join(path.read_text(errors="replace").splitlines()[:12])
    for m in BUILD_LINE.finditer(head):
        words = shlex.split(m.group(2))
        if path.name not in [Path(w).name for w in words] or "-o" not in words:
            continue
        if any("<" in w or ">" in w for w in words):
            return "placeholder"      # the line names something to fill in (a header folder, a library)
        cwd = ROOT if f"tools/{path.name}" in words else TOOLS
        i = words.index("-o")
        words[i + 1] = str(out)
        return [m.group(1)] + words, cwd
    return None


def run_one(path):
    """-> (name, state, detail): state is ok, FAIL, skip or norun."""
    name = path.name
    if name in NEEDS_INPUT:
        return name, "norun", NEEDS_INPUT[name]
    try:
        if path.suffix == ".py":
            r = subprocess.run([sys.executable, str(path)], cwd=ROOT, capture_output=True, text=True, timeout=TIMEOUT)
        elif path.suffix == ".sh":
            r = subprocess.run(["/bin/bash", str(path)], cwd=TOOLS, capture_output=True, text=True, timeout=TIMEOUT)
        else:
            with tempfile.TemporaryDirectory(prefix="nmtest-") as t:
                cmd = c_command(path, Path(t) / "t")
                if cmd == "placeholder":
                    return name, "norun", "its build line needs a path filled in (see its header)"
                if not cmd:
                    return name, "norun", "its header names no build line"
                b = subprocess.run(cmd[0], cwd=cmd[1], capture_output=True, text=True, timeout=TIMEOUT)
                if b.returncode:
                    return name, "FAIL", "does not build:\n" + (b.stdout + b.stderr)[-1200:]
                r = subprocess.run([str(Path(t) / "t")], cwd=cmd[1], capture_output=True, text=True, timeout=TIMEOUT)
    except subprocess.TimeoutExpired:
        return name, "FAIL", f"no result after {TIMEOUT} s"
    out = (r.stdout + r.stderr)
    if r.returncode == 0:
        return name, "ok", ""
    if r.returncode == 2 and "SKIP" in out:
        return name, "skip", out.strip().splitlines()[-1][:200]
    return name, "FAIL", f"exit {r.returncode}\n" + out[-1200:]


def main(argv):
    jobs = 4
    if argv[:1] == ["-j"]:
        jobs, argv = int(argv[1]), argv[2:]
    tests = sorted(p for p in TOOLS.glob("test_*") if p.suffix in (".py", ".c", ".cpp", ".sh"))
    if argv:
        tests = [p for p in tests if p.name in argv or p.stem in argv]
    if not tests:
        print("no test matched"); return 1
    with concurrent.futures.ThreadPoolExecutor(max_workers=jobs) as pool:
        results = list(pool.map(run_one, tests))
    by = {s: [(n, d) for n, st, d in results if st == s] for s in ("ok", "FAIL", "skip", "norun")}
    for n, d in by["FAIL"]:
        print(f"FAIL {n}: {d}\n")
    for n, d in by["skip"]:
        print(f"skip {n}: {d}")
    for n, d in by["norun"]:
        print(f"not run {n}: {d}")
    print(f"{len(by['ok'])} passed, {len(by['FAIL'])} failed, {len(by['skip'])} skipped, {len(by['norun'])} not run, of {len(results)}")
    return 1 if by["FAIL"] else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
