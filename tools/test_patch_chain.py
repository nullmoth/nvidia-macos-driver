#!/usr/bin/env python3
"""Every patch in kexts/NVRM/patches applies with --fuzz=0, in order, to a pristine open-gpu-kernel-modules 620.70.06.

The pristine tree is named by $NVRM_PRISTINE_OGKM. Without it the test says SKIP and exits 0, so it runs anywhere; a build host
sets the variable (as for build/build_nvrm.sh) and then it cannot pass silently. tools/test_patch_chain.sh does the work;
build/build_nvrm.sh runs the same check first when NVRM_PRISTINE_OGKM is set, so a drifted patch stops the build."""
import os, pathlib, subprocess, sys
root = pathlib.Path(__file__).resolve().parents[1]
tree = os.environ.get('NVRM_PRISTINE_OGKM', '')
if not os.path.isdir(tree):
    print('SKIP: NVRM_PRISTINE_OGKM does not name a pristine 620.70.06 tree'); sys.exit(0)
r = subprocess.run(['bash', str(root / 'tools/test_patch_chain.sh'), tree], capture_output=True, text=True)
print(r.stdout, end='')
patches = sorted(p.name for p in (root / 'kexts/NVRM/patches').glob('*.patch'))
ok = [l.split()[1] for l in r.stdout.splitlines() if l.startswith('OK')]
assert r.returncode == 0 and ok == patches, f'patch chain does not apply to the pristine tree: {r.stdout}{r.stderr}'
print(f'PASS: {len(patches)} patches apply with --fuzz=0 in order')
