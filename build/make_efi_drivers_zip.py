#!/usr/bin/env python3
"""make_efi_drivers_zip.py <out.zip> <X.kext>...: the package of NullMoth's own EFI drivers that the 1401 builder puts
into a PC's EFI switched off (experimental drivers a tester turns on in the Mac app).

Layout: Kexts/<X>.kext/... and SHA256SUMS ("<sha256 of the kext's executable>  <X>.kext" per line), the layout the
builder's VirtualSMC package already uses. The archive is the same bytes for the same kexts: entries sorted, one fixed
date, fixed permissions, no extra fields. Prints the archive's SHA-256 and size, which the builder pins."""
import hashlib, sys, zipfile
from pathlib import Path

DATE = (2026, 1, 1, 0, 0, 0)


def main(argv):
    if len(argv) < 2:
        print(__doc__); return 2
    out, kexts = Path(argv[0]), [Path(a) for a in argv[1:]]
    entries, sums = [], []
    for k in sorted(kexts, key=lambda p: p.name):
        name = k.name[:-5] if k.name.endswith(".kext") else None
        exe = k / "Contents/MacOS" / (name or "")
        if not name or not exe.is_file() or not (k / "Contents/Info.plist").is_file():
            print(f"STOP: {k} is not a complete kext"); return 1
        if any(p.is_symlink() for p in k.rglob("*")):
            print(f"STOP: {k} holds a link"); return 1
        sums.append(f"{hashlib.sha256(exe.read_bytes()).hexdigest()}  {k.name}")
        for f in sorted(p for p in k.rglob("*") if p.is_file()):
            entries.append((f"Kexts/{k.name}/{f.relative_to(k).as_posix()}", f.read_bytes(), 0o755 if f == exe else 0o644))
    entries.append(("SHA256SUMS", ("\n".join(sums) + "\n").encode(), 0o644))
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        for arc, data, mode in entries:
            zi = zipfile.ZipInfo(arc, DATE); zi.compress_type = zipfile.ZIP_DEFLATED; zi.external_attr = (0o100000 | mode) << 16
            z.writestr(zi, data)
    data = out.read_bytes()
    print(f"{out.name} sha256 {hashlib.sha256(data).hexdigest()} size {len(data)}")
    for s in sums:
        print("  " + s)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
