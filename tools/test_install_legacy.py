#!/usr/bin/env python3
"""The installer for the older-release kits, run end to end against a scratch folder.

install-legacy.sh runs on releases nobody here can start a card on, so its own logic is what can be proven: every
refusal happens before anything is changed, the right folder and cache tool are used per release, and nothing is
switched on that the script says it leaves off. System tools are stand-ins in a scratch bin/ that record their calls.

usage: test_install_legacy.py [repo root]"""
import hashlib, os, pathlib, shutil, stat, subprocess, sys, tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
SCRIPT = root / 'package/legacy/install-legacy.sh'
UNINST = root / 'package/legacy/uninstall-legacy.sh'
fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail and not ok else ''))
    fails += 0 if ok else 1


def tool(path, body):
    path.write_text('#!/bin/bash\n' + body)
    path.chmod(path.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)


def make(tmp, release, version_string, kexts=('NVRM', 'NVRMFB', 'NVAccel'), userland=True, nvidia=True, sip_off=True,
         boot_args='nvfb=1', linker_rc=0, kmutil_drop=''):
    """A kit folder and a scratch root with stand-in tools. Returns (kit, R)."""
    kit, R = tmp / 'kit', tmp / 'root'
    for k in kexts:
        d = kit / 'kexts' / f'{k}.kext/Contents/MacOS'; d.mkdir(parents=True)
        (d / k).write_bytes(b'kext ' + k.encode())
        (d.parent / 'Info.plist').write_text(f'<plist><string>com.nullmoth.{k}</string></plist>')
    if userland:
        d = kit / 'userland/payload/Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS'; d.mkdir(parents=True)
        (d / 'NVMTLDriver').write_bytes(b'plugin')
    (kit / 'opengl').mkdir(parents=True); (kit / 'opengl/MANIFEST.json').write_text('{}')
    (kit / 'RELEASE').write_text(release + '\n'); (kit / 'VERSION').write_text('3.0\n'); (kit / 'README.txt').write_text('kit\n')
    shutil.copy(SCRIPT, kit / 'install-legacy.sh'); shutil.copy(UNINST, kit / 'uninstall-legacy.sh')
    tool(kit / 'fw_fetch.sh', 'while [ $# -gt 0 ]; do case "$1" in --dest) D=$2; shift 2;; *) shift;; esac; done\n'
                              'mkdir -p "$D/nvidia/620" && echo fw > "$D/nvidia/620/gsp.bin" && echo "FW_OK $D"\n')
    sums = []
    for f in sorted(p for p in kit.rglob('*') if p.is_file()):
        sums.append(f'{hashlib.sha256(f.read_bytes()).hexdigest()}  ./{f.relative_to(kit)}')
    (kit / 'SHA256SUMS').write_text('\n'.join(sums) + '\n')
    b = R / 'bin'; b.mkdir(parents=True)
    for d in ('Library/Extensions', 'System/Library/Extensions', 'Library/NullMoth'):
        (R / d).mkdir(parents=True)
    log = R / 'calls.log'
    tool(b / 'sw_vers', f'echo "{version_string}"\n')
    tool(b / 'uname', 'echo x86_64\n')
    tool(b / 'ioreg', ('echo \'    "vendor-id" = <de100000>\'\n' if nvidia else 'echo \'    "vendor-id" = <86800000>\'\n'))
    tool(b / 'csrutil', 'echo "System Integrity Protection status: %s."\n' % ('disabled' if sip_off else 'enabled'))
    tool(b / 'nvram', f'echo "boot-args\t{boot_args}"\n')
    tool(b / 'kextutil', f'echo "kextutil $*" >> "{log}"; exit {linker_rc}\n')
    tool(b / 'kextcache', f'echo "kextcache $*" >> "{log}"; exit 0\n')
    tool(b / 'kmutil', f'echo "kmutil $*" >> "{log}"\n'
         'if [ "$1" = create ]; then while [ $# -gt 0 ]; do [ "$1" = -A ] && { echo kc > "$2"; }; shift; done; exit 0; fi\n'
         'if [ "$1" = inspect ]; then for k in NVRM NVRMFB NVRMAGDC NVAccel; do '
         f'[ "$k" = "{kmutil_drop}" ] || echo "com.nullmoth.$k"; done; exit 0; fi\n')
    return kit, R


def run(kit, R, *args):
    env = dict(os.environ, NM_LEGACY_TEST_ROOT=str(R), TMPDIR=str(R))
    p = subprocess.run(['/bin/bash', str(kit / 'install-legacy.sh'), *args], env=env, capture_output=True, text=True)
    return p.returncode, p.stdout + p.stderr


def changed(R):
    """Whether anything was placed outside the stand-in tools and the call log."""
    return [str(p.relative_to(R)) for p in R.rglob('*') if p.is_file() and p.parts[len(R.parts)] != 'bin' and p.name != 'calls.log']


if os.geteuid() == 0:
    print('skip: the test seam is closed for root, as it must be'); sys.exit(0)
with tempfile.TemporaryDirectory() as t:
    t = pathlib.Path(t)

    kit, R = make(t / 'a', '10.13', '10.13.6')
    rc, out = run(kit, R)
    calls = (R / 'calls.log').read_text()
    check('10.13: installs and says so', rc == 0 and 'RESULT installed' in out, out[-300:])
    check('10.13: kexts go to /Library/Extensions', all((R / f'Library/Extensions/{k}.kext/Contents/MacOS/{k}').exists() for k in ('NVRM', 'NVRMFB', 'NVAccel')))
    check('10.13: every kext was test-linked before anything was copied', calls.count('kextutil -n -t') == 3 and calls.index('kextutil') < calls.index('kextcache'))
    check('10.13: the cache is rebuilt with kextcache -i', 'kextcache -i' in calls and 'kmutil' not in calls)
    check('10.13: the Metal files and the firmware are placed', (R / 'Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/NVMTLDriver').exists()
          and (R / 'Library/NullMoth/nvfw/nvidia/620/gsp.bin').exists())
    check('10.13: the parts left off are kept beside the driver with the uninstaller',
          (R / 'Library/NullMoth/legacy/10.13/opengl/MANIFEST.json').exists() and (R / 'Library/NullMoth/legacy/10.13/uninstall-legacy.sh').exists())
    check('10.13: the kext folder is recorded as the real path, not the scratch one',
          (R / 'Library/NullMoth/legacy/10.13/kext-folder').read_text().strip() == '/Library/Extensions')
    check('10.13: boot arguments are not changed, and the accelerator switch is explained', 'nvaccel=1' in out and 'nvram' not in calls)

    kit, R = make(t / 'b', '10.8', '10.8.5', kexts=('NVRM', 'NVRMFB'), userland=False)
    rc, out = run(kit, R)
    calls = (R / 'calls.log').read_text()
    check('10.8: kexts go to /System/Library/Extensions (no /Library/Extensions before 10.9)',
          rc == 0 and (R / 'System/Library/Extensions/NVRM.kext').is_dir() and not (R / 'Library/Extensions/NVRM.kext').exists(), out[-300:])
    check('10.8: the cache is rebuilt with kextcache -u, and no accelerator switch is offered', 'kextcache -u' in calls and 'nvaccel=1' not in out)

    kit, R = make(t / 'c', '12', '12.7.6')
    rc, out = run(kit, R)
    calls = (R / 'calls.log').read_text()
    check('12: a test kernel collection is built and read back before anything is copied',
          rc == 0 and calls.count('kmutil create') == 2 and 'kmutil inspect' in calls and 'kextcache' not in calls, out[-300:])
    check('12: the auxiliary collection is published', (R / 'Library/KernelCollections/AuxiliaryKernelExtensions.kc').exists())
    check('12: no missing-kit flag before 13', '--allow-missing-kdk' not in calls)
    kit, R = make(t / 'c2', '14', '14.8')
    rc, out = run(kit, R)
    check('14: the collection tool is told it may work without a debug kit', rc == 0 and '--allow-missing-kdk' in (R / 'calls.log').read_text())

    for name, kw, vs, want in (
            ('a kit for another release is refused', {}, '10.14.6', 'built for macOS 10.13'),
            ('a PC with no NVIDIA card is refused', {'nvidia': False}, '10.13.6', 'no NVIDIA card'),
            ('system protection left on is refused (10.11 and later)', {'sip_off': False}, '10.13.6', 'System Integrity Protection'),
            ('a kext this system\'s linker refuses stops the install', {'linker_rc': 1}, '10.13.6', 'linker refused')):
        kit, R = make(t / ('r' + str(abs(hash(name)))), '10.13', vs, **kw)
        rc, out = run(kit, R)
        check(name + ', and nothing is changed', rc != 0 and want in out and 'RESULT stopped' in out and not changed(R), out[-200:] + str(changed(R))[:200])
    kit, R = make(t / 'd', '10.10', '10.10.5', kexts=('NVRM', 'NVRMFB'), userland=False, boot_args='nvfb=1')
    rc, out = run(kit, R)
    check('10.10 without kext-dev-mode=1 is refused, and nothing is changed', rc != 0 and 'kext-dev-mode=1' in out and not changed(R))
    kit, R = make(t / 'd2', '10.10', '10.10.5', kexts=('NVRM', 'NVRMFB'), userland=False, boot_args='nvfb=1 kext-dev-mode=1')
    rc, out = run(kit, R)
    check('10.10 with kext-dev-mode=1 installs', rc == 0 and 'RESULT installed' in out, out[-200:])
    kit, R = make(t / 'e', '13', '13.7.8', kmutil_drop='NVAccel')
    rc, out = run(kit, R)
    check('13: a kext the collection tool leaves out stops the install, and nothing is changed',
          rc != 0 and 'left out NVAccel' in out and not changed(R), out[-200:])
    kit, R = make(t / 'f', '10.13', '10.13.6')
    (kit / 'kexts/NVRM.kext/Contents/MacOS/NVRM').write_bytes(b'tampered')
    rc, out = run(kit, R)
    check('a kit whose files do not match SHA256SUMS is refused', rc != 0 and 'do not match SHA256SUMS' in out and not changed(R))
    kit, R = make(t / 'g', '10.13', '10.13.6')
    rc, out = run(kit, R, '--check')
    check('--check tests the kit and installs nothing', rc == 0 and 'check passed' in out and not changed(R), str(changed(R))[:200])
    # the seam itself: without the scratch bin/ the variable is ignored and the real refusals apply
    kit, R = make(t / 'h', '10.13', '10.13.6')
    shutil.rmtree(R / 'bin')
    p = subprocess.run(['/bin/bash', str(kit / 'install-legacy.sh')], env=dict(os.environ, NM_LEGACY_TEST_ROOT=str(R)), capture_output=True, text=True)
    check('control: without stand-in tools the script runs for real and refuses this Mac', p.returncode != 0 and 'RESULT stopped' in p.stdout, p.stdout[-200:])
print('test_install_legacy: %s' % ('OK' if not fails else f'{fails} FAILED'))
sys.exit(1 if fails else 0)
