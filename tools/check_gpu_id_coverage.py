#!/usr/bin/env python3
"""check_gpu_id_coverage.py: prove that every Intel/AMD GPU and NPU PCI id real users report is bound by the
upstream id tables the ports compile. Field ids come from the 1401 scan-evidence and macOS hardware-map uploads
(tools/gpu_field_ids.json). The id tables are upstream Linux/Mesa source and live on the build host, so their
paths are given as arguments; on the build Mac they are under its build folder (build/gpu-src).

Coverage model (checked against the 6.14/drm-kmod sources on 2026-10-14):
  Intel i915/xe  - explicit lists. An id is covered when it is reachable (transitively) from an INTEL_*_IDS macro
                   that i915_pci.c or xe_pci.c instantiates. No wildcard: a missing id is a real gap.
  AMD amdgpu     - PCI_DEVICE(0x1002, PCI_ANY_ID) + CHIP_IP_DISCOVERY binds every 0x1002 VGA/display/accel function
                   (GFX9+ read their IP table from the GPU). RADV/radeonsi read the family from the kernel, so they
                   follow amdgpu; there is no userspace id list to extend for GFX9+. A literal list is kept only for
                   pre-GFX9 (SI/CIK) parts. So AMD coverage is a property of the wildcard, not of any id.
  NPU            - ivpu (Intel) and amdxdna (AMD) each carry an explicit id list; a missing id is a real gap.

Exit status is non-zero if any genuine gap is found, so a build host can gate on it.
"""
import argparse, glob, json, re, sys, os


def read(p):
    """Read a file, or concatenate every .c/.h under a directory (ivpu keeps its id table and the
    PCI_DEVICE_ID_* defines in different files of one directory)."""
    if not p or not os.path.exists(p):
        return ""
    if os.path.isdir(p):
        return "\n".join(open(f).read() for f in sorted(glob.glob(os.path.join(p, '*.[ch]'))))
    return open(p).read()


def intel_supported(pciids_path, i915_path, xe_path):
    """Union of device ids reachable from the INTEL_*_IDS macros that i915_pci.c and xe_pci.c instantiate."""
    pci = read(pciids_path)
    direct, refs = {}, {}
    for m in re.finditer(r'#define\s+(INTEL_\w+_IDS)\s*\([^)]*\)([\s\S]*?)(?=\n#define|\n#endif|\Z)', pci):
        name, body = m.group(1), m.group(2)
        direct[name] = set(x.lower() for x in re.findall(r'0x([0-9A-Fa-f]{4})', body))
        refs[name] = set(re.findall(r'\b(INTEL_\w+_IDS)\b', body))

    def resolve(name, seen):
        if name in seen:
            return set()
        seen.add(name)
        ids = set(direct.get(name, set()))
        for r in refs.get(name, set()):
            ids |= resolve(r, seen)
        return ids

    covered = set()
    for src in (i915_path, xe_path):
        s = read(src)
        for name in direct:
            if re.search(r'\b' + name + r'\b', s):
                covered |= resolve(name, set())
    return covered


def amd_wildcard(amdgpu_path):
    return 'PCI_DEVICE(0x1002, PCI_ANY_ID)' in read(amdgpu_path)


def amd_literal(amdgpu_path):
    return set(m.group(1).lower() for m in re.finditer(r'PCI_DEVICE\(0x1002,\s*0x([0-9A-Fa-f]{4})', read(amdgpu_path)))


def npu_supported(ivpu_path, amdxdna_path, pciids_path):
    """Explicit NPU id lists. ivpu names ids through PCI_DEVICE_ID_* macros (defined in a drm pciids header);
    amdxdna lists 0x1022 ids directly. Resolve ivpu macro names to hex from the given headers."""
    hdr = read(ivpu_path) + read(pciids_path)
    macro_hex = dict(re.findall(r'#define\s+(PCI_DEVICE_ID_\w+)\s+0x([0-9A-Fa-f]{4})', hdr))
    covered = set()
    tbl = read(ivpu_path)
    for m in re.finditer(r'PCI_DEVICE\([^,]+,\s*(PCI_DEVICE_ID_\w+)\)', tbl):
        h = macro_hex.get(m.group(1))
        if h:
            covered.add('8086:' + h.lower())
    for m in re.finditer(r'PCI_DEVICE\(PCI_VENDOR_ID_AMD,\s*0x([0-9A-Fa-f]{4})', read(amdxdna_path)):
        covered.add('1022:' + m.group(1).lower())
    return covered


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--field', default=os.path.join(os.path.dirname(__file__), 'gpu_field_ids.json'))
    ap.add_argument('--pciids'); ap.add_argument('--i915'); ap.add_argument('--xe')
    ap.add_argument('--amdgpu'); ap.add_argument('--ivpu'); ap.add_argument('--amdxdna')
    # ids that are reported in the field but are not real Intel/AMD GPUs (VM/basic-display adapters, misreads):
    ap.add_argument('--ignore', default='8086:fd80')
    a = ap.parse_args()
    field = json.load(open(a.field))
    ignore = set(x.strip() for x in a.ignore.split(',') if x.strip())

    intel = intel_supported(a.pciids, a.i915, a.xe)
    amd_any = amd_wildcard(a.amdgpu)
    amd_lit = amd_literal(a.amdgpu)
    npu = npu_supported(a.ivpu, a.amdxdna, a.pciids)

    gaps = []
    for did in field.get('intel_gpu', {}):
        dev = did.split(':')[1]
        if did not in ignore and dev not in intel:
            gaps.append(('intel i915/xe', did, field['intel_gpu'][did]['name']))
    for did in field.get('amd_gpu', {}):
        dev = did.split(':')[1]
        if not (amd_any or dev in amd_lit) and did not in ignore:
            gaps.append(('amdgpu', did, field['amd_gpu'][did]['name']))
    for did in field.get('npu', {}):
        if did not in npu and did not in ignore:
            gaps.append(('npu ivpu/amdxdna', did, field['npu'][did]['name']))

    print(f"intel ids supported: {len(intel)}   amd wildcard: {amd_any}   npu ids supported: {len(npu)}")
    if gaps:
        print(f"GAPS ({len(gaps)}): field ids not bound by any id table:")
        for drv, did, name in gaps:
            print(f"  {drv:18} {did}  {name}")
        return 1
    n = len(field.get('intel_gpu', {})) + len(field.get('amd_gpu', {})) + len(field.get('npu', {}))
    print(f"OK: every field id ({n}) is bound by the compiled id tables (ignored: {sorted(ignore)})")
    return 0


if __name__ == '__main__':
    sys.exit(main())
