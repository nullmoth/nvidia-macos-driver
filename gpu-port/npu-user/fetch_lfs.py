#!/usr/bin/env python3
"""fetch_lfs.py <git checkout> <owner/repo> <path prefix>...: replaces git-LFS pointer files under the prefixes with
their content, without git-lfs, from GitHub's media server at the checkout's commit.

The NPU compiler embeds its prebuilt SHAVE kernels (sw_runtime_kernels/*/prebuild, LFS) into every compiled model; a
checkout without LFS content leaves pointers there and compilation stops at "Incorrect ELF magic". Each file is checked
against the sha256 and size its pointer names before it replaces the pointer; anything that does not match stays a
pointer and is reported (exit 1).
"""
import concurrent.futures
import hashlib
import os
import subprocess
import sys
import urllib.request

root, repo, prefixes = sys.argv[1], sys.argv[2], sys.argv[3:]
commit = subprocess.run(["git", "-C", root, "rev-parse", "HEAD"], capture_output=True, text=True, check=True).stdout.strip()
files = subprocess.run(["git", "-C", root, "ls-files", "--", *prefixes], capture_output=True, text=True,
                       check=True).stdout.split("\n")


def pointer(path):
    try:
        with open(path, "rb") as f:
            head = f.read(400)
    except OSError:
        return None
    if not head.startswith(b"version https://git-lfs.github.com/spec/"):
        return None
    fields = dict(l.split(" ", 1) for l in head.decode().splitlines() if " " in l)
    return fields["oid"].split(":", 1)[1], int(fields["size"])


def fetch(rel):
    path = os.path.join(root, rel)
    p = pointer(path)
    if p is None:
        return None
    oid, size = p
    url = f"https://media.githubusercontent.com/media/{repo}/{commit}/{urllib.request.quote(rel)}"
    data = urllib.request.urlopen(url, timeout=60).read()
    if len(data) != size or hashlib.sha256(data).hexdigest() != oid:
        return f"MISMATCH {rel}"
    with open(path + ".lfs-tmp", "wb") as f:
        f.write(data)
    os.replace(path + ".lfs-tmp", path)
    return "ok"


bad = 0
done = 0
with concurrent.futures.ThreadPoolExecutor(16) as ex:
    for rel, r in zip(files, ex.map(lambda f: fetch(f) if f else None, files)):
        if r is None:
            continue
        if r == "ok":
            done += 1
        else:
            bad += 1
            print(r)
print(f"fetch_lfs: {done} files replaced, {bad} refused, commit {commit[:12]}")
sys.exit(1 if bad else 0)
