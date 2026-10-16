#!/usr/bin/env python3
"""The opt-in AMD/Intel GPU driver (setup --gpu-test): the firmware list and the GPU detection it relies on."""
import os, pathlib, re, subprocess, sys, tempfile

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
arm("only GPU, NPU and licence files are fetched",
    all(p.startswith(("amdgpu/", "i915/", "xe/", "intel/vpu/", "amdnpu/", "LICENSES/")) and ".." not in p for p in paths))
arm("the NPU licences travel with the NPU firmware",
    "LICENSES/LICENSE.intel_vpu" in paths and "LICENSES/LICENSE.amdnpu" in paths)

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
arm("turning it on refuses a Mac with neither GPU", 'stop "this Mac has no AMD or Intel GPU for this driver (' in src)
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

# NPUs: Intel's processing accelerator (class 12) and AMD's signal-processing NPU (1180) are found, nothing else
npu_awk = re.search(r"NPUS=\$\(ioreg -r -c IOPCIDevice -l -w0 \| LC_ALL=C awk -F'<\|>' '(.*?)' \| sort -u\)", src, re.S)
npu_sample = """+-o NPU0@b  <class IOPCIDevice>
    | "class-code" = <00001200>
    | "vendor-id" = <86800000>
+-o AMDN@0  <class IOPCIDevice>
    | "vendor-id" = <22100000>
    | "class-code" = <00801100>
+-o HDEF@0  <class IOPCIDevice>
    | "vendor-id" = <22100000>
    | "class-code" = <00030400>
+-o SIO@0  <class IOPCIDevice>
    | "vendor-id" = <86800000>
    | "class-code" = <00801100>
"""
npus = subprocess.run(["awk", "-F<|>", npu_awk.group(1).replace("\n    ", " ")], input=npu_sample, capture_output=True,
                      text=True).stdout.split() if npu_awk else []
arm("an Intel NPU and an AMD NPU are found; AMD audio and an Intel 1180 device are not", sorted(npus) == ["1022", "8086"],
    " ".join(npus))
# firmware links: made inside one directory only, a target outside it is refused
link_loop = re.search(r"(    grep '\^#link ' \"\$MAN\" \| while read -r _ n _ tgt; do\n.*?\n    done\n)", src, re.S)
with tempfile.TemporaryDirectory() as d:
    ft = pathlib.Path(d, "ft"); (ft / "amdnpu/1502_00").mkdir(parents=True)
    (ft / "amdnpu/1502_00/npu.sbin.1.5.5.391").write_text("fw")
    man = pathlib.Path(d, "man.txt")
    man.write_text("#link amdnpu/1502_00/npu_7.sbin -> npu.sbin.1.5.5.391\n#link amdnpu/1502_00/evil.sbin -> ../../etc\n"
                   "#link ../escape -> npu.sbin.1.5.5.391\n")
    r = subprocess.run(["bash", "-c", link_loop.group(1) if link_loop else "exit 9"],
                       env={"MAN": str(man), "FT": str(ft), "PATH": "/usr/bin:/bin"}, capture_output=True, text=True)
    good = (ft / "amdnpu/1502_00/npu_7.sbin").is_symlink() and os.readlink(ft / "amdnpu/1502_00/npu_7.sbin") == "npu.sbin.1.5.5.391"
    bad = (ft / "amdnpu/1502_00/evil.sbin").exists() or os.path.lexists(pathlib.Path(d, "escape"))
    arm("firmware links are made within their directory; traversal is refused", r.returncode == 0 and good and not bad)
# AMD NPU validation archives (VTD): one pinned commit, every file hashed, one family per NPU
vt_lines = (ROOT / "app/Resources/nullmoth-npu-tests.txt").read_text().splitlines()
vt_commit = re.search(r"Xilinx/VTD/([0-9a-f]{40})/", vt_lines[1]) if len(vt_lines) > 1 else None
vt = [l.split() for l in vt_lines if l and not l.startswith("#")]
arm("the NPU validation list pins one VTD commit and hashes every file", bool(vt_commit) and len(vt) == 4 and all(
    len(e) == 3 and re.fullmatch(r"[0-9a-f]{64}", e[0]) and ".." not in e[2] for e in vt))
arm("the NPU validation list covers Phoenix, Strix, NPU3 and AMD's licence",
    sorted(e[1] for e in vt) == ["license", "npu3", "phx", "strx"])
amd_awk = re.search(r"AMDNPU=\$\(ioreg -r -c IOPCIDevice -l -w0 \| LC_ALL=C awk -F'<\|>' '(.*?)' \| sort -u\)", src, re.S)
fam_loop = re.search(r"(for d in \$AMDNPU; do case \$d in .*?esac; done)", src)
amd_sample = """+-o NPU0@b  <class IOPCIDevice>
    | "class-code" = <00001200>
    | "device-id" = <3d640000>
    | "vendor-id" = <86800000>
+-o PHX@0  <class IOPCIDevice>
    | "class-code" = <00801100>
    | "device-id" = <02150000>
    | "vendor-id" = <22100000>
+-o STX@0  <class IOPCIDevice>
    | "class-code" = <00801100>
    | "device-id" = <f0170000>
    | "vendor-id" = <22100000>
+-o HDEF@0  <class IOPCIDevice>
    | "class-code" = <00030400>
    | "device-id" = <e3150000>
    | "vendor-id" = <22100000>
"""
ids = subprocess.run(["awk", "-F<|>", amd_awk.group(1).replace("\n      ", " ")], input=amd_sample, capture_output=True,
                     text=True).stdout.split() if amd_awk else []
arm("the AMD NPUs' device ids are read (Phoenix 1502, Strix 17f0); the Intel NPU and AMD audio are not",
    sorted(ids) == ["1502", "17f0"], " ".join(ids))
fams = subprocess.run(["bash", "-c", (fam_loop.group(1) if fam_loop else "exit 9") + " | sort -u"],
                      env={"AMDNPU": "1502 17f0 1b0b 1022", "PATH": "/usr/bin:/bin"}, capture_output=True, text=True).stdout.split()
arm("each AMD NPU maps to its VTD family; an unknown id fetches nothing", fams == ["npu3", "phx", "strx"], " ".join(fams))
vt_block = re.search(r"(    if \[ -n \"\$AMDNPU\" \] && .*?\n    fi\n)", src, re.S)
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d)
    srv = d / "srv"; (srv / "archive/phx").mkdir(parents=True)
    (srv / "archive/phx/xrt_smi_phx.a").write_bytes(b"good")
    (srv / "LICENSE.vtd").write_bytes(b"tampered")
    import hashlib
    man = d / "tests.txt"
    man.write_text("# VTD\n# fetched from https://raw.githubusercontent.com/Xilinx/VTD/" + "a" * 40 + "/<path>\n"
                   + hashlib.sha256(b"good").hexdigest() + " phx archive/phx/xrt_smi_phx.a\n"
                   + hashlib.sha256(b"original").hexdigest() + " license LICENSE.vtd\n"
                   + hashlib.sha256(b"good").hexdigest() + " phx ../../escape\n")
    (d / "bin").mkdir(); (d / "ext/NMXNPU.kext").mkdir(parents=True)
    # curl stand-in: serves the file named after the commit in the URL from srv/
    (d / "bin/curl").write_text("#!/bin/bash\nout=\"\"; url=\"\"\nwhile [ $# -gt 0 ]; do case $1 in -o) out=$2; shift 2;; "
                                "-*) shift;; *) url=$1; shift;; esac; done\nf=${url#*/VTD/}; f=${f#*/}\n"
                                f"cp \"{srv}/$f\" \"$out\"\n")
    (d / "bin/curl").chmod(0o755)
    vb = d / "bins"
    script = ("note() { echo \"NOTE $*\"; }\n" + vt_block.group(1)) if vt_block else "exit 9"
    r = subprocess.run(["bash", "-c", script], capture_output=True, text=True,
                       env={"AMDNPU": "1502", "EXT": str(d / "ext"), "VT": str(man), "VB": str(vb),
                            "PATH": f"{d}/bin:/usr/bin:/bin", "TOOL": ""})
    got = sorted(p.name for p in vb.iterdir()) if vb.exists() else []
    arm("the matching archive installs; a tampered licence and a path escape are refused",
        got == ["xrt_smi_phx.a"] and "LICENSE.vtd" in r.stdout and not (d / "escape").exists(), f"{got} {r.stdout} {r.stderr}")
arm("turning it off removes the NPU validation archives", "rm -rf \"$GFW\" /Library/NullMoth/npu/xrt/share/amdxdna/bins" in src)
# Send logs: the NPU report finds NPUs whatever order ioreg prints their properties in
col_awk = re.search(r"ioreg -r -c IOPCIDevice -l -w0 2>/dev/null \| LC_ALL=C awk '\n(      function out\(\).*?END \{ out\(\) \})'",
                    src, re.S)
col_sample = amd_sample + """+-o SIO@0  <class IOPCIDevice>
    | "class-code" = <00801100>
    | "device-id" = <11110000>
    | "vendor-id" = <86800000>
"""
col = subprocess.run(["awk", col_awk.group(1)], input=col_sample, capture_output=True, text=True).stdout if col_awk else ""
arm("the log report lists the Intel and both AMD NPUs, not AMD audio or an Intel 1180 device",
    all(n in col for n in ("NPU0@b", "PHX@0", "STX@0")) and "HDEF@0" not in col and "SIO@0" not in col)
arm("the log report runs the NPU probes as the logged-in user, each bounded in time",
    'sudo -u "$console_user" "$@" 2>&1 & p=$!' in src and 'kill -9 "$p"' in src and '> "$COLLECT/npu.txt"' in src)
# install replaces the NPU userland but keeps the validation archives setup fetched for this Mac
inst = (ROOT / "package/install.sh").read_text()
npu_inst = re.search(r"(if \[ -d \"\$HERE/Library/NullMoth/npu\" \]; then\n.*?\nfi\n)", inst, re.S)
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d)
    live, pkg = d / "live", d / "pkg"
    (live / "npu/xrt/share/amdxdna/bins").mkdir(parents=True)
    (live / "npu/xrt/share/amdxdna/bins/xrt_smi_phx.a").write_text("fetched")
    (live / "npu/lib").mkdir(); (live / "npu/lib/old.dylib").write_text("old")
    (pkg / "Library/NullMoth/npu/lib").mkdir(parents=True); (pkg / "Library/NullMoth/npu/lib/new.dylib").write_text("new")
    # the live tree moves into the sandbox; the package source ($HERE/Library/NullMoth) keeps its layout
    block = (npu_inst.group(1).replace("$HERE/Library/NullMoth", "@PKG@").replace("/Library/NullMoth", str(live))
             .replace("@PKG@", "$HERE/Library/NullMoth").replace("/private/var/tmp", str(d))) if npu_inst else "exit 9"
    r = subprocess.run(["bash", "-c", "die() { echo \"DIE $*\"; exit 1; }\n" + block], capture_output=True, text=True,
                       env={"HERE": str(pkg), "BK": "bk", "PATH": "/usr/bin:/bin"})
    kept = (live / "npu/xrt/share/amdxdna/bins/xrt_smi_phx.a").is_file()
    arm("an update replaces the NPU userland and keeps the fetched validation archives",
        r.returncode == 0 and kept and (live / "npu/lib/new.dylib").is_file() and not (live / "npu/lib/old.dylib").exists(),
        r.stdout + r.stderr)
unin = (ROOT / "package/uninstall.sh").read_text()
arm("uninstall removes the NPU driver too", 'KEXTS="NVRM NVAccel NVRMFB NVRMAGDC NMXGPU NMXAccel NMXNPU"' in unin)
arm("turning it off clears the NPU opt-in", "ga del nmxnpu=1" in src)


# An Intel GPU switched off in OpenCore is not on macOS's PCI list (Lilu terminates it), so the scan finds no GPU: "on"
# turns it back on and "off" puts back exactly what was removed (2.0.3, Core Ultra laptop, chat photo 10-16).
def seg(start, end):
    a = src.index(start); return src[a:src.index(end, a)]
decide = seg('  REEN=/Library/NullMoth/gputest-reenabled; IGOFF=""', '  if [ "$GPUTEST" = on ]; then\n    [ -n "$WANT" ]')
apply_ = seg('    if [ -n "$IGOFF" ]; then\n      mkdir', "    # the drivers' boot guards")
undo = seg('    # an Intel or AMD GPU that "on" turned back on', '    if [ -d "$EXT/NMXGPU.kext" ]')
BASE = """<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>DeviceProperties</key><dict><key>Add</key><dict><key>%s</key><dict>%s</dict></dict></dict>
<key>NVRAM</key><dict><key>Add</key><dict><key>7C436110-AB2A-4BBB-A880-FE41995C9F82</key><dict><key>boot-args</key><string>%s</string></dict></dict></dict></dict></plist>"""


INTEL, AMD = "PciRoot(0x0)/Pci(0x2,0x0)", "PciRoot(0x0)/Pci(0x8,0x1)/Pci(0x0,0x0)"


def run(cpu, props, bootargs, vendors="", want="", path=INTEL):
    with tempfile.TemporaryDirectory() as t:
        c = os.path.join(t, "config.plist"); open(c, "w").write(BASE % (path, props, bootargs))
        bkc = c + ".bak"; open(bkc, "w").write(open(c).read())
        harness = f'''C="{c}"; B=7C436110-AB2A-4BBB-A880-FE41995C9F82; BKC="{bkc}"; WANT="{want}"; VENDORS="{vendors}"
has() {{ plutil -extract "$1" raw -o - "$C" >/dev/null 2>&1; }}; get() {{ plutil -extract "$1" raw -o - "$C" 2>/dev/null; }}
note() {{ echo "NOTE $*"; }}; ok() {{ echo "OK $*"; }}; stop() {{ echo "STOP $*"; exit 1; }}
sysctl() {{ echo {cpu}; }}; nvram() {{ :; }}
ga() {{ local a new=""; for a in $(get NVRAM.Add.$B.boot-args); do [ "$a" = "$2" ] || new="$new $a"; done
  [ "$1" = add ] && new="$new $2"; plutil -replace NVRAM.Add.$B.boot-args -string "${{new# }}" "$C"; }}
GPUTEST=on
''' + decide.replace("REEN=/Library/NullMoth/gputest-reenabled", f"REEN={t}/reen") + '''
echo "WANT=[$WANT] VENDORS=[$VENDORS] IGOFF=[$IGOFF]"
''' + apply_.replace("mkdir -p /Library/NullMoth", "true") + '''
plutil -convert xml1 -o "$C.on" "$C"
GPUTEST=off
''' + undo
        r = subprocess.run(["bash", "-c", harness], capture_output=True, text=True)
        on = open(c + ".on").read() if os.path.exists(c + ".on") else ""
        js = lambda p: subprocess.run(["plutil", "-convert", "json", "-o", "-", p], capture_output=True, text=True).stdout
        return r.stdout + r.stderr, on, js(c) == js(bkc), os.path.exists(os.path.join(t, "reen"))


def bootargs(xml):
    m = re.search(r"<key>boot-args</key>\s*<string>(.*?)</string>", xml); return m.group(1) if m else "?"


out, on, same, left = run("GenuineIntel", "<key>disable-gpu</key><data>AQAAAA==</data>", "-v keepsyms=1")
arm("an Intel GPU off by disable-gpu is wanted for the test driver", "WANT=[ -nmxintel]" in out and "VENDORS=[ 8086]" in out, out.splitlines()[0] if out else "")
arm("'on' takes disable-gpu off the Intel GPU", "disable-gpu" not in on and "boot-args" in on)
arm("'off' puts the config back exactly as it was", same and not left, out[-300:])
out, on, same, left = run("GenuineIntel", "", "-v -wegnoigpu")
arm("-wegnoigpu counts as a switched-off Intel GPU", "IGOFF=[-wegnoigpu]" in out)
arm("'on' removes -wegnoigpu and 'off' adds it back", "-wegnoigpu" not in bootargs(on) and same, bootargs(on))
out, on, same, left = run("AuthenticAMD", "<key>disable-gpu</key><data>AQAAAA==</data>", "-v", path=AMD)
arm("an AMD APU GPU off by disable-gpu (behind 00:08.1) is wanted and turned back on", "WANT=[ -nmxamd]" in out and "VENDORS=[ 1002]" in out and "disable-gpu" not in on, out.splitlines()[0] if out else "")
arm("'off' switches the AMD GPU off again exactly as it was", same and not left)
out, on, same, left = run("AuthenticAMD", "<key>disable-gpu</key><data>AQAAAA==</data>", "-v", path=INTEL)
arm("an AMD Mac does not touch a disable-gpu at Intel's address", "WANT=[]" in out and "disable-gpu" in on)
out, on, same, left = run("AuthenticAMD", "", "-v -wegnoigpu", path=AMD)
arm("-wegnoigpu is Intel's switch and is left alone on AMD", "WANT=[]" in out and "-wegnoigpu" in bootargs(on))
out, on, same, left = run("GenuineIntel", "<key>disable-gpu</key><data>AQAAAA==</data>", "-v", want=" -nmxnpu")
arm("an NPU does not hide a switched-off Intel GPU (Core Ultra 135U)", "WANT=[ -nmxnpu -nmxintel]" in out and "disable-gpu" not in on, out.splitlines()[0] if out else "")
out, on, same, left = run("GenuineIntel", "<key>disable-gpu</key><data>AQAAAA==</data>", "-v", vendors=" 8086", want=" -nmxintel")
arm("a visible Intel GPU leaves the config alone", "IGOFF=[]" in out and "disable-gpu" in on)
# the whole script must parse in the bash it runs under (/bin/bash 3.2): the pieces above run alone, so a quote opened in
# a stop message (2.0.4 draft: "${IGPU:-the CPU's GPU}") passed every arm while setup could not start at all
for sh in ("/bin/bash", "bash"):
    r = subprocess.run([sh, "-n", str(SETUP)], capture_output=True, text=True)
    arm(f"setup parses in {sh}", r.returncode == 0, r.stderr.strip()[:160])
bad = src.replace('${IGPU:-the processor GPU}', "${IGPU:-the CPU's GPU}")
r = subprocess.run(["/bin/bash", "-n"], input=bad, capture_output=True, text=True)
arm("negative control: the apostrophe version does not parse", r.returncode != 0)
print(f"{len(fails)} failed")
sys.exit(1 if fails else 0)
