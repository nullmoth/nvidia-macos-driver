#!/usr/bin/env python3
"""scrub_kit.py <kit dir>: takes the build machine's folder names out of an assembled legacy kit before it is packed.

The 3.0 legacy kits carried the build machine's folder in their link log (kexts/link-test.txt) and in four userland
binaries (the translator, the Vulkan loader, the video encoder shim, the OS-version shim): source paths a compiler
records for panics and debug messages. The public package must name no machine of ours, so each prefix is replaced
by a neutral one of the same length (a binary's offsets do not move), and every changed Mach-O is signed again ad hoc
with its identifier and entitlements kept. Exit 1 if any prefix is still present afterwards."""
import os
import subprocess
import sys

import re

# A volume or home folder at the start of a recorded source path: /Volumes/<name> or /Users/<name>. /Users/Shared is
# a real place the installers use, and scripts are left alone (they name /Volumes/* on purpose); only binaries and the
# link log carry build paths.
PREFIX = re.compile(rb"/(?:Volumes|Users)/(?!Shared\b)[A-Za-z0-9._-]+")


def neutral(m):
    n = len(m.group(0))
    return (b"/private/tmp/" + b"x" * n)[:n] if n >= 13 else b"/" + b"x" * (n - 1)


def scrub_bytes(data):
    return PREFIX.sub(neutral, data)


def is_macho(data):
    return data[:4] in (b"\xcf\xfa\xed\xfe", b"\xce\xfa\xed\xfe", b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca")


def main(root):
    changed = []
    for base, _dirs, files in os.walk(root):
        for name in files:
            path = os.path.join(base, name)
            if os.path.islink(path):
                continue
            with open(path, "rb") as fh:
                data = fh.read()
            if not (is_macho(data) or name == "link-test.txt"):
                continue
            new = scrub_bytes(data)
            if new == data:
                continue
            tmp = path + ".scrub"
            with open(tmp, "wb") as fh:
                fh.write(new)
            os.chmod(tmp, os.stat(path).st_mode & 0o7777)
            os.replace(tmp, path)
            if is_macho(new):
                r = subprocess.run(["codesign", "--force", "--sign", "-", "--preserve-metadata=identifier,entitlements",
                                    path], capture_output=True, text=True)
                if r.returncode:
                    print(f"STOP: could not sign {path} again: {r.stderr.strip()}")
                    return 1
            changed.append(os.path.relpath(path, root))
    left = []
    for base, _dirs, files in os.walk(root):
        for name in files:
            path = os.path.join(base, name)
            if os.path.islink(path):
                continue
            with open(path, "rb") as fh:
                data = fh.read()
            if (is_macho(data) or name == "link-test.txt") and PREFIX.search(data):
                left.append(os.path.relpath(path, root))
    for c in changed:
        print(f"scrubbed {c}")
    if left:
        print("STOP: build paths remain in " + ", ".join(left))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1]))
