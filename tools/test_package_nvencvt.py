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
        sig = subprocess.run(['codesign', '-v', str(m / 'libnvencvt.dylib')], capture_output=True, text=True)
        check('the staged library is signed', sig.returncode == 0, sig.stderr.strip()[-80:])
else:
    print('skip: staging arms need a Mac with clang')
print(f'{fails} failed' if fails else 'all passed'); sys.exit(1 if fails else 0)
