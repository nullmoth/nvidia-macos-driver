#!/usr/bin/env python3
"""Replay exact source changes in a temporary directory; never modify inputs."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def check_files(root, files, stage):
    for relative, hashes in files.items():
        if digest(root / relative) != hashes[stage]:
            raise RuntimeError(f"{stage} source differs: {relative}")


def replay(source, destination, files, patches):
    check_files(source, files, "before")
    for relative in files:
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source / relative, target)
    for patch in patches:
        subprocess.run(["git", "apply", "--check", str(HERE / patch)],
                       cwd=destination, check=True)
        subprocess.run(["git", "apply", str(HERE / patch)],
                       cwd=destination, check=True)
    check_files(destination, files, "after")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--clvk-tree", type=Path, required=True,
                        help="Clean clvk at the pinned commit")
    parser.add_argument("--mesa-tree", type=Path, required=True,
                        help="Mesa after the pinned NullMoth 3.0 + DLSS preparation")
    parser.add_argument("--cpu-tests", action="store_true",
                        help="Also compile and run the supplied CPU mock regressions")
    args = parser.parse_args()
    pins = json.loads((HERE / "sources.json").read_text())
    for relative, expected in pins["patches_sha256"].items():
        if digest(HERE / relative) != expected:
            raise RuntimeError(f"Patch differs: {relative}")
    with tempfile.TemporaryDirectory(prefix="opencl-source-replay-") as temp:
        root = Path(temp)
        clvk, mesa = root / "clvk", root / "mesa"
        replay(args.clvk_tree, clvk, pins["clvk_touched_files"],
               ["patches/clvk-private-opencl.patch"])
        replay(args.mesa_tree, mesa, pins["mesa_touched_files"],
               ["patches/mesa-queue-barrier-ownership.patch",
                "patches/nvk-selective-spill-retry.patch"])
        if args.cpu_tests:
            for script, options in (
                ("test_calibration.py", ["--clvk-tree", clvk]),
                ("test_selector.py", ["--original", args.mesa_tree,
                                      "--candidate", mesa]),
                ("test_queue_ownership.py", ["--original", args.mesa_tree,
                                             "--candidate", mesa]),
            ):
                subprocess.run([sys.executable, str(HERE / script),
                                *map(str, options)], check=True)
    print("PASS: patches replay to the recorded sources; input trees unchanged")


if __name__ == "__main__":
    main()
