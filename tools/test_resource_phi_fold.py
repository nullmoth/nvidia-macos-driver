#!/usr/bin/env python3
"""Run the production resource-phi pass and IR tests without the unpublished native-test fixtures.

Build the translator first, or pass --deps pointing to Cargo's compiled dependency directory.
Only Ctx's unused fields are omitted; the pass, SPIR-V IR, parser and dominator code are included unchanged.
"""
import argparse
import json
import pathlib
import subprocess
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--deps', type=pathlib.Path)
args = parser.parse_args()
candidates = [args.deps] if args.deps else [
    ROOT / 'out/translator/x86_64-apple-darwin/release/deps',
    ROOT / 'out/translator-wrapper-tests/x86_64-apple-darwin/release/deps',
]
library = next((p for d in candidates for p in sorted(d.glob('libspirv-*.rlib'))), None)
if not library:
    raise SystemExit('Build the translator first; its spirv dependency is needed by this test.')
source = ROOT / 'translator/translator/src'
modules = ''.join('#[path=' + json.dumps(str(source / filename)) + '] mod ' + name + ';\n'
                  for name, filename in [('fxhash', 'fxhash.rs'), ('float16', 'float16.rs'), ('spirv_operand', 'spirv_operand.rs'),
                                         ('spirv_binary', 'spirv_binary/mod.rs'), ('spirv_module', 'spirv_module.rs'),
                                         ('dominators', 'dominators.rs')])
code = '#![allow(dead_code)]\n' + modules + '''
mod passes {
    use crate::spirv_module::{Instruction, Module};
    struct Ctx { module: Module, new_globals: Vec<Instruction> }
    impl Ctx { fn new(module: Module) -> Self { Self { module, new_globals: vec![] } } }
    #[path=PATH] mod resource_phi_fold;
}
'''.replace('PATH', json.dumps(str(source / 'passes/resource_phi_fold.rs')))
with tempfile.TemporaryDirectory() as directory:
    path = pathlib.Path(directory)
    (path / 'check.rs').write_text(code)
    subprocess.run(['rustc', '--edition=2021', '--test', '-O', str(path / 'check.rs'),
                    '--extern', 'spirv=' + str(library), '-L', 'dependency=' + str(library.parent),
                    '-o', str(path / 'check')], check=True)
    subprocess.run([str(path / 'check'), 'resource_phi_fold'], check=True)
