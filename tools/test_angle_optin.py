#!/usr/bin/env python3
"""Execute the production constructor and verify default policy and explicit overrides."""
import os
import pathlib
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
source = (root / 'plugin/NVMTLDevice.m').read_text()
start = source.index('__attribute__((constructor)) static void nvmtl_angle_allow_nvidia(void)')
end = source.index('static bool nvmtl_process_is_allowed(void)', start)
constructor = source[start:end]
with tempfile.TemporaryDirectory() as directory:
    file = pathlib.Path(directory) / 'policy.c'
    exe = pathlib.Path(directory) / 'policy'
    file.write_text('#include <stdio.h>\n#include <stdlib.h>\n#include <string.h>\n' + constructor + '\nint main(void) { const char *v = getenv("ANGLE_FEATURE_OVERRIDES_DISABLED"); puts(v ? v : "<unset>"); return 0; }\n')
    subprocess.run(['cc', str(file), '-o', str(exe)], check=True)
    cases = [(None, None, 'disableMetalOnNvidia'), ('0', None, '<unset>'), ('0', 'otherRule', 'otherRule'),
             (None, 'otherRule', 'otherRule:disableMetalOnNvidia'), ('1', None, 'disableMetalOnNvidia'),
             ('1', 'otherRule:disableMetalOnNvidia', 'otherRule:disableMetalOnNvidia')]
    for opt, override, expected in cases:
        env = dict(os.environ)
        for key, value in [('NVMTL_ANGLE_METAL', opt), ('ANGLE_FEATURE_OVERRIDES_DISABLED', override)]:
            env.pop(key, None)
            if value is not None:
                env[key] = value
        actual = subprocess.check_output([str(exe)], env=env, text=True).strip()
        assert actual == expected, (opt, override, actual, expected)
print('ANGLE policy: on by default, off with NVMTL_ANGLE_METAL=0, existing overrides kept, in six cases.')
