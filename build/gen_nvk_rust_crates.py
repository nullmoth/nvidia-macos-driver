#!/usr/bin/env python3
"""Write the licence notice for the Rust crates Mesa fetches while building NVK.

usage: gen_nvk_rust_crates.py <mesa tree>/subprojects/packagecache <out file>

NAK (the NVK shader compiler) is built from Rust crates that Mesa's subprojects/*.wrap files download into
packagecache. Their copyright and licence texts must ship with the binary. Every archive in packagecache is read
(never extracted to disk): crate name, version, licence expression and repository come from its Cargo.toml, and every
LICENSE / COPYING / NOTICE file at the crate root is copied in full. An archive with no licence file stops the run, so
a crate cannot be shipped without its text."""
import os
import re
import sys
import tarfile

LIC = re.compile(r"^(LICEN[CS]E|COPYING|NOTICE|UNLICENSE)([-_.].*)?$", re.I)


def cargo_field(toml, key):
    m = re.search(r"^%s\s*=\s*\"([^\"]*)\"" % re.escape(key), toml, re.M)
    return m.group(1) if m else None


def main():
    cache, out = sys.argv[1], sys.argv[2]
    names = sorted(n for n in os.listdir(cache) if n.endswith((".tar.gz", ".tgz")))
    if not names:
        sys.exit("STOP: no crate archives in %s (did the meson setup fetch them?)" % cache)
    parts = [
        "Rust crates linked into the NullMoth NVK Vulkan driver (libvulkan_nouveau.dylib, through NAK).\n"
        "Each is used under the licence named for it (the permissive alternative where a crate offers several).\n"
        "Mesa's own licence is in licenses/Mesa-license.rst.\n"
    ]
    rule = "=" * 100
    for n in names:
        with tarfile.open(os.path.join(cache, n)) as t:
            members = {m.name: m for m in t.getmembers() if m.isfile()}
            tops = {k.split("/")[0] for k in members}
            assert len(tops) == 1, "%s: expected one top directory" % n
            top = tops.pop()
            toml = t.extractfile(members[top + "/Cargo.toml"]).read().decode("utf-8", "replace")
            pkg = toml.split("[package]", 1)[1].split("\n[", 1)[0] if "[package]" in toml else toml
            name = cargo_field(pkg, "name") or top
            ver = cargo_field(pkg, "version") or "?"
            lic = cargo_field(pkg, "license") or "?"
            repo = cargo_field(pkg, "repository") or cargo_field(pkg, "homepage") or ""
            files = sorted(k for k in members if k.count("/") == 1 and LIC.match(k.split("/")[1]))
            if not files:
                sys.exit("STOP: %s has no licence file" % n)
            parts.append("\n%s\n%s %s   licence: %s\n%s\n%s\n" % (rule, name, ver, lic, repo, rule))
            for k in files:
                parts.append("--- %s ---\n%s\n" % (k.split("/")[1], t.extractfile(members[k]).read().decode("utf-8", "replace").rstrip()))
    tmp = out + ".tmp"
    with open(tmp, "w") as f:
        f.write("".join(parts))
    os.replace(tmp, out)
    print("nvk-rust-crates: %d crates -> %s" % (len(names), out))


main()
