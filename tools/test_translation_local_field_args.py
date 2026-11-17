#!/usr/bin/env python3
"""Check local aggregate addresses passed to typed helper parameters.

Run with a built translator; --baseline verifies that the old translator rejects
the implicit first-field cases. All sources below are independent small examples.
"""
import argparse
import json
from pathlib import Path
import subprocess
import sys
import tempfile

from test_translation_resource_reads import translate


def source(nested=False, explicit=False, scalar=False, direct=False):
    value_type = 'float' if scalar else '<3 x float>'
    value = '2.0' if scalar else '<float 1.0, float 2.0, float 3.0>'
    record = '%Outer' if nested else '%Record'
    allocation = value_type if direct else record
    indices = ', i32 0, i64 0, i32 0' if nested else ', i32 0'
    field = (f'%field = getelementptr inbounds {record}, ptr %slot, i64 0{indices}'
             if explicit else '')
    argument = '%field' if explicit else '%slot'
    return f'''
%Record = type {{ {value_type}, float, [12 x i8] }}
%Outer = type {{ [2 x %Record], i32 }}
define void @kernel_main(ptr addrspace(1) %out) {{
entry:
  %slot = alloca {allocation}, align 16
  {field}
  call void @write_field(ptr {argument})
  %v = call {value_type} @read_field(ptr {argument})
  store {value_type} %v, ptr addrspace(1) %out, align 16
  ret void
}}
define internal void @write_field(ptr %p) {{
entry:
  store {value_type} {value}, ptr %p, align 16
  ret void
}}
define internal {value_type} @read_field(ptr %p) {{
entry:
  %v = load {value_type}, ptr %p, align 16
  ret {value_type} %v
}}
!air.kernel = !{{!0}}
!0 = !{{ptr @kernel_main, !1, !2}}
!1 = !{{}}
!2 = !{{!3}}
!3 = !{{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1,
        !"air.write", !"air.address_space", i32 1,
        !"air.arg_type_name", !"float3", !"air.arg_name", !"out"}}
'''


def checks(library):
    results = {}
    cases = {'vector': {}, 'nested_vector': {'nested': True},
             'scalar': {'scalar': True}, 'explicit_field': {'explicit': True},
             'direct_vector': {'direct': True}}
    with tempfile.TemporaryDirectory(prefix='nvmtl-local-field-') as tmp:
        for name, options in cases.items():
            path = Path(tmp) / (name + '.ll')
            path.write_text(source(**options))
            try:
                data = translate(library, path, 'kernel')
            except RuntimeError as error:
                # Only the measured pointer/value contract failure is a valid
                # negative control, not an unrelated parse or tool failure.
                if 'pointer-pointee and value-type contract' not in str(error):
                    raise
                results[name] = False
                continue
            spv = path.with_suffix('.spv'); spv.write_bytes(data)
            subprocess.run(['spirv-val', '--target-env', 'vulkan1.2',
                            '--scalar-block-layout', str(spv)], check=True)
            results[name] = True
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('library', type=Path)
    parser.add_argument('--baseline', type=Path)
    parser.add_argument('--json', action='store_true')
    args = parser.parse_args()
    result = checks(args.library)
    if args.json:
        print(json.dumps(result)); return
    assert all(result.values()), result
    print('fixed:', result)
    if args.baseline:
        run = subprocess.run([sys.executable, __file__, str(args.baseline), '--json'],
                             check=True, capture_output=True, text=True)
        before = json.loads(run.stdout)
        assert before == dict(vector=False, nested_vector=False, scalar=False,
                              explicit_field=True, direct_vector=True), before
        print('baseline:', before)


if __name__ == '__main__':
    main()
