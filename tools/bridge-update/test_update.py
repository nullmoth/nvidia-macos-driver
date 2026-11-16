#!/usr/bin/env python3
"""Exercise the real updater against isolated copies, including failed transactions."""
import argparse
import fcntl
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

REPO = Path(__file__).resolve().parents[2]
PROVIDER = 'Library/GPUBundles/nvmtl/wine'
BACKUP = 'Library/Application Support/NVK-Bridge-Updates/3.0.1-nvk-hybrid-1'


def hashes(root):
    return {str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in sorted(root.rglob('*')) if p.is_file() and not p.is_symlink()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base-provider', type=Path, required=True)
    parser.add_argument('--package', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    package = args.package.resolve()
    spec = json.loads((package / 'UPDATE.json').read_text())
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    helper = output / 'test-updater'
    command = ['xcrun', 'clang++', '-arch', 'x86_64', '-mmacosx-version-min=15.5',
               '-std=c++17', '-fobjc-arc', '-O1', '-g', '-fsanitize=address,undefined',
               '-Wall', '-Wextra', '-Werror', '-Wno-deprecated-declarations',
               '-DNVK_BRIDGE_UPDATE_TEST=1', str(REPO / 'tools/bridge-update/update.mm'),
               '-framework', 'Foundation', '-framework', 'Security', '-o', str(helper)]
    build = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    (output / 'build.log').write_text(build.stdout)
    assert not build.returncode, build.stdout
    results = []

    def fixture(parent):
        root = parent / 'system'
        shutil.copytree(args.base_provider, root / PROVIDER, symlinks=True)
        assert hashes(root / PROVIDER) == spec['before']
        return root

    def run(root, action, *, result=0, options=(), source=package):
        command = [str(helper), action, '--package', str(source), '--root', str(root), *map(str, options)]
        p = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, timeout=45)
        with (output / 'transactions.log').open('a') as log:
            log.write(action + ' ' + ' '.join(map(str, options)) + f' => {p.returncode}\n' + p.stdout)
        assert p.returncode == result, p.stdout
        assert 'AddressSanitizer' not in p.stdout and 'runtime error:' not in p.stdout, p.stdout
        return p.stdout

    def passed(name):
        results.append(name)
        print('PASS', name, flush=True)

    with tempfile.TemporaryDirectory(prefix='fixtures-', dir=output) as temporary:
        parent = Path(temporary)
        root = fixture(parent / 'roundtrip')
        run(root, 'check')
        assert not (root / BACKUP).exists(), 'check created a backup'
        run(root, 'install')
        assert hashes(root / PROVIDER) == spec['after']
        for name in ('dxvk/x64/d3d11.dll', 'dxvk/x64/dxgi.dll', 'vkd3d/x64/d3d12.dll',
                     'vkd3d/x64/d3d12core.dll', 'lib/libnvk_renderer.dylib'):
            assert spec['after'][name] == spec['before'][name]
        assert (root / PROVIDER / 'lib/libMoltenVK.dylib').readlink() == Path('libnvk_wine.dylib')
        state = (root / BACKUP / 'state.json').read_bytes()
        backup_files = hashes(root / BACKUP)
        run(root, 'install')
        run(root, 'check')
        assert (root / BACKUP / 'state.json').read_bytes() == state
        assert hashes(root / BACKUP) == backup_files
        assert hashes(root / PROVIDER) == spec['after']
        run(root, 'rollback')
        assert hashes(root / PROVIDER) == spec['before']
        run(root, 'rollback')
        run(root, 'install')
        assert hashes(root / PROVIDER) == spec['after']
        passed('install, identical install, check, rollback, identical rollback and reinstall')

        # The unchanged runtime helper must accept all updated provider hashes.
        p = subprocess.run([str(root / PROVIDER / 'bin/nvk-runtime'), 'check', str(root / PROVIDER)],
                           stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, timeout=20)
        (output / 'provider-check.log').write_text(p.stdout)
        assert p.returncode == 0, p.stdout
        passed('unchanged provider helper accepts the updated manifest')

        for written in (1, 3, 5):
            trial = fixture(parent / f'failure-{written}')
            run(trial, 'install', result=1, options=('--fail-after', written))
            assert hashes(trial / PROVIDER) == spec['before']
        passed('automatic rollback after early, middle and final replacement failures')

        for written, action in ((1, 'install'), (3, 'rollback'), (5, 'rollback')):
            trial = fixture(parent / f'interrupted-{written}')
            run(trial, 'install', result=86, options=('--crash-after', written))
            run(trial, action)
            assert hashes(trial / PROVIDER) == spec['after' if action == 'install' else 'before']
        passed('journal recovery after interrupted installs, including all-new files')

        trial = fixture(parent / 'newer-install')
        run(trial, 'install')
        changed = trial / PROVIDER / 'nvk-wine-runtime.json'
        changed.write_bytes(changed.read_bytes() + b'\n')
        snapshot = hashes(trial / PROVIDER)
        run(trial, 'rollback', result=1)
        assert hashes(trial / PROVIDER) == snapshot
        passed('rollback preserves a subsequent provider modification')

        trial = fixture(parent / 'modified-baseline')
        changed = trial / PROVIDER / 'dxvk/x64/d3d11.dll'
        changed.write_bytes(changed.read_bytes() + b'changed')
        snapshot = hashes(trial / PROVIDER)
        run(trial, 'install', result=1)
        assert hashes(trial / PROVIDER) == snapshot
        passed('unexpected baseline files are refused without replacement')

        trial = fixture(parent / 'damaged-backup')
        run(trial, 'install')
        changed = trial / BACKUP / 'originals/lib/libnvk_wine.dylib'
        changed.write_bytes(b'broken backup')
        snapshot = hashes(trial / PROVIDER)
        run(trial, 'rollback', result=1)
        assert hashes(trial / PROVIDER) == snapshot
        passed('damaged rollback backup is refused before changing the provider')

        trial = fixture(parent / 'bad-alias')
        link = trial / PROVIDER / 'lib/libMoltenVK.dylib'
        link.unlink()
        link.symlink_to('libnvk_renderer.dylib')
        snapshot = hashes(trial / PROVIDER)
        run(trial, 'install', result=1)
        assert hashes(trial / PROVIDER) == snapshot
        passed('unexpected Vulkan aliases are refused')

        trial = fixture(parent / 'payload-integrity')
        bad = parent / 'tampered-package'
        shutil.copytree(package, bad)
        file = bad / 'payload/lib/libnvk_wine.dylib'
        file.write_bytes(b'broken executable')
        run(trial, 'install', result=1, source=bad)
        assert hashes(trial / PROVIDER) == spec['before']
        file.unlink()
        file.symlink_to(package / 'payload/lib/libnvk_wine.dylib')
        run(trial, 'install', result=1, source=bad)
        assert hashes(trial / PROVIDER) == spec['before']
        passed('tampered and symlinked package payloads are refused')

        trial = fixture(parent / 'busy-provider')
        held = subprocess.Popen([sys.executable, '-u', '-c',
            'import ctypes,sys; ctypes.CDLL(sys.argv[1]); print("loaded",flush=True); sys.stdin.read()',
            str(trial / PROVIDER / 'lib/libnvk_wine.dylib')],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            assert held.stdout.readline().strip() == 'loaded'
            message = run(trial, 'install', result=1)
            assert 'Close all Wine/CrossOver/YAAGL' in message
            assert hashes(trial / PROVIDER) == spec['before']
        finally:
            held.communicate('', timeout=10)
        run(trial, 'install')
        passed('running provider blocks replacement; install succeeds after it exits')

        trial = fixture(parent / 'concurrent-update')
        lock_path = trial / BACKUP / 'lock'
        lock_path.parent.mkdir(parents=True)
        with lock_path.open('w') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            message = run(trial, 'install', result=1)
            assert 'Another bridge update is running' in message
        assert hashes(trial / PROVIDER) == spec['before']
        passed('concurrent installer lock prevents overlapping transactions')

        trial = fixture(parent / 'linked-provider')
        lib = trial / PROVIDER / 'lib'
        outside = parent / 'linked-libraries'
        lib.rename(outside)
        lib.symlink_to(outside)
        run(trial, 'install', result=1)
        assert hashes(outside) == {name.removeprefix('lib/'): digest for name, digest
                                  in spec['before'].items() if name.startswith('lib/')}
        passed('linked provider directories are refused without changing their target')

    (output / 'RESULTS.json').write_text(json.dumps({'passed': results, 'count': len(results)}, indent=2) + '\n')


if __name__ == '__main__':
    main()
