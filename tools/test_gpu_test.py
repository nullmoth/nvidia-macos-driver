#!/usr/bin/env python3
"""The opt-in AMD/Intel GPU driver (setup --gpu-test): the firmware list and the GPU detection it relies on."""
import pathlib, re, subprocess, sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
MAN = ROOT / "app/Resources/nullmoth-gpu-firmware.txt"
SETUP = ROOT / "app/Resources/nullmoth-setup.sh"
fails = []


def arm(name, ok, detail=""):
    print(("ok   " if ok else "FAIL ") + name + (f" ({detail})" if detail else ""))
    if not ok:
        fails.append(name)


lines = MAN.read_text().splitlines()
commit = re.search(r"raw/([0-9a-f]{40})/", lines[1])
arm("the firmware list pins one linux-firmware commit", bool(commit))
entries = [l.split() for l in lines if not l.startswith("#")]
arm("every firmware line is a SHA-256 and a path", all(len(e) == 2 and re.fullmatch(r"[0-9a-f]{64}", e[0]) for e in entries),
    f"{len(entries)} lines")
paths = [e[1] for e in entries]
arm("the AMD and Intel licences travel with the firmware",
    "LICENSES/LICENSE.amdgpu" in paths and "LICENSES/LICENSE.i915" in paths and "LICENSES/LICENSE.xe" in paths)
arm("only amdgpu, i915, xe and licence files are fetched",
    all(p.startswith(("amdgpu/", "i915/", "xe/", "LICENSES/")) and ".." not in p for p in paths))

# the awk setup runs over ioreg: each device's vendor and class are paired even when class-code comes first
awk = re.search(r"LC_ALL=C awk -F'<\|>' '(.*?)' \| sort -u\)", SETUP.read_text(), re.S).group(1).replace("\n    ", " ")
sample = """+-o PXSX@0  <class IOPCIDevice>
    | "class-code" = <00000300>
    | "vendor-id" = <02100000>
+-o GFX0@0  <class IOPCIDevice>
    | "vendor-id" = <86800000>
    | "class-code" = <00000300>
+-o XHC@14  <class IOPCIDevice>
    | "vendor-id" = <86800000>
    | "class-code" = <30030c00>
+-o NIC@0  <class IOPCIDevice>
    | "class-code" = <00000200>
    | "vendor-id" = <ec100000>
"""
out = subprocess.run(["awk", "-F<|>", awk], input=sample, capture_output=True, text=True).stdout.split()
arm("an AMD GPU listed class-first is found", "1002" in out, " ".join(out))
arm("an Intel GPU listed vendor-first is found", "8086" in out)
arm("a USB controller from the same vendor does not count as a GPU", out.count("8086") == 1)
arm("a network card is not a GPU", "10ec" not in out)
src = SETUP.read_text()
arm("turning it on refuses a Mac with neither GPU", 'stop "this Mac has no AMD or Intel GPU for this driver"' in src)
arm("a checksum mismatch stops before anything is installed",
    src.index("did not match its checksum") < src.index('ditto "$GK" "$EXT/NMXGPU.kext"'))

# Metal on the AMD/Intel GPU: setup puts NMXAccel and the Vulkan drivers in, and "off" takes out exactly those files
import tempfile
arm("turning it on installs the Metal accelerator and checks the kernel collection took it",
    'ditto "$GA" "$EXT/NMXAccel.kext"' in src and "grep -q com.nullmoth.NMXAccel" in src)
rm_line = next((l.strip() for l in src.splitlines() if 'while read -r f; do rm -f "$GB/nvmtl/${f#./}"' in l), "")
with tempfile.TemporaryDirectory() as d:
    gu, nv = pathlib.Path(d, "gpu-userland"), pathlib.Path(d, "GPUBundles/nvmtl")
    for f in ("libvulkan_radeon.dylib", "radeon_icd.json", "share/libdrm/amdgpu.ids"):
        (gu / f).parent.mkdir(parents=True, exist_ok=True); (gu / f).write_text("x")
        (nv / f).parent.mkdir(parents=True, exist_ok=True); (nv / f).write_text("x")
    for f in ("libvulkan_nouveau.dylib", "nvk_icd.json"):
        (nv / f).write_text("nvk")
    r = subprocess.run(["bash", "-c", rm_line], env={"GU": str(gu), "GB": str(nv.parent), "PATH": "/usr/bin:/bin"},
                       capture_output=True, text=True)
    left = sorted(str(x.relative_to(nv)) for x in nv.rglob("*") if x.is_file())
    arm("turning it off removes the AMD/Intel Vulkan drivers and leaves NVK's files", bool(rm_line) and r.returncode == 0
        and left == ["libvulkan_nouveau.dylib", "nvk_icd.json"], " ".join(left))
plugin = (ROOT / "plugin/nvmtl_vk.c").read_text()
arm("the Metal plugin loads RADV for AMD and ANV for Intel, NVK otherwise",
    'case 0x1002: return "/Library/GPUBundles/nvmtl/libvulkan_radeon.dylib"' in plugin
    and 'case 0x8086: return "/Library/GPUBundles/nvmtl/libvulkan_intel.dylib"' in plugin
    and 'default:     return "/Library/GPUBundles/nvmtl/libvulkan_nouveau.dylib"' in plugin)
arm("NVIDIA machine code is never dispatched on an AMD/Intel device",
    "if (nvmtl_vk_vendor() != 0x10de) return -3;" in plugin)
print(f"{len(fails)} failed")
sys.exit(1 if fails else 0)
