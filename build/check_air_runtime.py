#!/usr/bin/env python3
"""Check the linked AIR runtime's dyld interface, not only its nlist symbol table."""
import re
import subprocess
import sys
from pathlib import Path


def required_api_names():
    root = Path(__file__).resolve().parents[1]
    source = "\n".join((root / name).read_text() for name in
                       ("plugin/NVMTLLibraryLoad.m", "plugin/air_dis.c"))
    # Include literal dlsym names, the walk table and NV_LLVM_LOAD macro calls.
    # Optional optimizer entry points are required in a complete release too.
    return set(re.findall(r'"(LLVM\w+)"', source)) | set(re.findall(r'NV_LLVM_LOAD\((LLVM\w+)\)', source))


def check(path):
    exports = subprocess.check_output(["xcrun", "dyld_info", "-exports", str(path)], text=True)
    names = re.findall(r"^\s+0x[0-9a-fA-F]+\s+(_\S+)", exports, re.M)
    required = {"_" + name for name in required_api_names()}
    missing = sorted(required - set(names))
    if missing:
        raise ValueError("AIR runtime lacks required LLVM C entry points: " + ", ".join(missing))
    extra = [n for n in names if not n.startswith("_LLVM")]
    if extra:
        raise ValueError("AIR runtime exports internal symbols: " + ", ".join(extra[:3]))
    fixups = subprocess.check_output(["xcrun", "dyld_info", "-fixups", str(path)], text=True)
    # Hidden exports alone do not repair a previously linked dylib's weak-bind records.
    if re.search(r"<weak-def-coalesce>/.*(?:4llvm|5polly)", fixups):
        raise ValueError("AIR runtime still coalesces internal LLVM symbols")


if __name__ == "__main__":
    try:
        check(sys.argv[1])
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        sys.exit("STOP: " + str(error))
    print("AIR runtime exposes only its LLVM C interface")
