#!/usr/bin/env python3
"""A package must carry libnvencvt.dylib: install.sh stops before changing anything when it is missing, and
package/stage_nvencvt.sh puts a checked library into a package tree and into SHA256SUMS.

usage: test_package_nvencvt.py [repo root]   (the staging arms need a Mac with clang)"""
import pathlib, platform, shutil, subprocess, sys, tempfile
root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
fails = 0
def check(n, ok, d=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + n + (f' ({d})' if d else '')); fails += 0 if ok else 1
inst = (root / 'package/install.sh').read_text()
i = inst.find('libnvencvt.dylib" ] ||')
check('install.sh stops when the package has no libnvencvt.dylib', i > 0 and 'die "this package has no libnvencvt.dylib' in inst[i:i + 300])
first_change = min(x for x in (inst.find('INSTALLING=1'), inst.find('ditto "$HERE/Library/GPUBundles/$b"')) if x > 0)
check('the check comes before the install changes anything', 0 < i < first_change)
stage = root / 'package/stage_nvencvt.sh'
if stage.exists() and platform.system() == 'Darwin' and shutil.which('clang'):
    with tempfile.TemporaryDirectory() as d:
        d = pathlib.Path(d)
        pkg = d / 'pkgroot'; m = pkg / 'Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS'; m.mkdir(parents=True)
        (pkg / 'SHA256SUMS').write_text('0000  ./other\n1111  ./Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/libnvencvt.dylib\n')
        good = d / 'good.c'; good.write_text('int NVEncVT_Register(int r){return r;} int NVEncVT_RegisterDecoders(int r){return r;} int NVEncVT_CreateInstance(int a){return a;} int NVDecVT_CreateInstance(int a){return a;}\n')
        bad = d / 'bad.c'; bad.write_text('int NVEncVT_Register(int r){return r;}\n')
        arch = ['-arch', 'x86_64']
        for n, src in (('good', good), ('bad', bad)):
            subprocess.run(['clang', *arch, '-dynamiclib', '-o', str(d / f'{n}.dylib'), str(src)], check=True, capture_output=True)
        r = subprocess.run(['bash', str(stage), str(pkg), str(d / 'bad.dylib')], capture_output=True, text=True)
        check('a library missing entry points is refused', r.returncode != 0 and 'does not export' in r.stdout, r.stdout.strip()[-80:])
        r = subprocess.run(['bash', str(stage), str(d / 'nope'), str(d / 'good.dylib')], capture_output=True, text=True)
        check('a path that is not a package tree is refused', r.returncode != 0)
        r = subprocess.run(['bash', str(stage), str(pkg), str(d / 'good.dylib')], capture_output=True, text=True)
        check('a good library is staged', r.returncode == 0 and (m / 'libnvencvt.dylib').exists(), (r.stdout + r.stderr).strip()[-120:])
        sums = (pkg / 'SHA256SUMS').read_text()
        lines = [l for l in sums.split('\n') if 'libnvencvt' in l]
        check('SHA256SUMS keeps its other lines and has exactly one fresh libnvencvt line', './other' in sums and len(lines) == 1 and '1111' not in sums)
        chk = root / 'package/check_package.sh'
        # the package gate also needs the Ryzen power parts (package/stage_nmsmn.sh); stage a minimal pair here
        import hashlib, plistlib
        k = d / 'NMSMN.kext/Contents'; (k / 'MacOS').mkdir(parents=True)
        (k / 'Info.plist').write_bytes(plistlib.dumps({'CFBundleIdentifier': 'com.nullmoth.NMSMN'})); (k / 'MacOS/NMSMN').write_bytes(b'k')
        (d / 'ryzenadj').write_bytes(b'b')
        subprocess.run(['bash', str(root / 'package/stage_nmsmn.sh'), str(d / 'NMSMN.kext'), str(d / 'ryzenadj'),
                        hashlib.sha256(b'b').hexdigest(), str(pkg)], check=True, capture_output=True)
        # install.sh runs sums_complete.sh, so the gate wants it in the tree and listed (as finish_package.sh does)
        shutil.copy(root / 'package/sums_complete.sh', pkg / 'sums_complete.sh')
        (m / 'nvmtl-air-dis').write_bytes(b'#!/bin/sh\n'); (m / 'nvmtl-air-dis').chmod(0o755)
        subprocess.run(['bash', str(root / 'package/sums_refresh.sh'), str(pkg), 'Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/nvmtl-air-dis'], check=True, capture_output=True)
        (pkg / 'other').write_text('o')
        (pkg / 'SHA256SUMS').write_text((pkg / 'SHA256SUMS').read_text().replace('0000  ./other', hashlib.sha256(b'o').hexdigest() + '  ./other'))
        subprocess.run(['bash', str(root / 'package/sums_refresh.sh'), str(pkg), 'sums_complete.sh'], check=True, capture_output=True)
        r = subprocess.run(['bash', str(chk), str(pkg)], capture_output=True, text=True)
        check('the package check passes a staged tree', r.returncode == 0, r.stdout.strip()[-80:])
        (m / 'libnvencvt.dylib').write_bytes((m / 'libnvencvt.dylib').read_bytes() + b'x')
        r = subprocess.run(['bash', str(chk), str(pkg)], capture_output=True, text=True)
        check('it fails when the library no longer matches SHA256SUMS or its signature', r.returncode != 0, r.stdout.strip()[-80:])
        (m / 'libnvencvt.dylib').unlink()
        r = subprocess.run(['bash', str(chk), str(pkg)], capture_output=True, text=True)
        check('it fails when the library is missing', r.returncode != 0 and 'missing' in r.stdout, r.stdout.strip()[-80:])
        subprocess.run(['bash', str(stage), str(pkg), str(d / 'good.dylib')], capture_output=True)
        sig = subprocess.run(['codesign', '-v', str(m / 'libnvencvt.dylib')], capture_output=True, text=True)
        check('the staged library is signed', sig.returncode == 0, sig.stderr.strip()[-80:])
else:
    print('skip: staging arms need a Mac with clang')
print(f'{fails} failed' if fails else 'all passed'); sys.exit(1 if fails else 0)
