#!/usr/bin/env python3
"""The repository's top level holds only the directories and files the project is made of. A stray copy of the build,
dlss and nvk folders once rode in with an unrelated commit (114 files under one extra top-level folder); nothing built
from it, but a second, older copy of a build script is a trap for whoever edits the wrong one."""
import subprocess, sys
from pathlib import Path

root = Path(__file__).resolve().parents[1]
ALLOWED = {".github", ".gitignore", "AGENTS.md", "CHANGELOG.md", "CONTRIBUTING.md", "DEVELOPING.md", "LICENSE", "NOTICE", "README.md", "SECURITY.md",
           "build", "dlss", "docs", "gl", "kexts", "licenses", "nvk", "package", "plugin", "tools", "translator", "vendor"}


def top_level(names):
    return sorted({n.split("/", 1)[0] for n in names if n})


def unexpected(names):
    return [t for t in top_level(names) if t not in ALLOWED]


failures = []
def arm(name, ok, detail=""):
    print(("ok   " if ok else "FAIL ") + name + (f": {detail}" if detail and not ok else ""))
    if not ok: failures.append(name)

tracked = subprocess.run(["git", "-C", str(root), "ls-files"], capture_output=True, text=True, check=True).stdout.splitlines()
arm("the listing is not empty (the check looked at something)", len(tracked) > 500, str(len(tracked)))
arm("every tracked top-level entry is one the project is made of", not unexpected(tracked), ", ".join(unexpected(tracked)))
arm("control: a stray top-level folder is reported", unexpected(tracked + ["ssh_dummy/build/build_nvk.sh"]) == ["ssh_dummy"])
if failures:
    print("FAILED:", ", ".join(failures)); sys.exit(1)
print("PASS")
