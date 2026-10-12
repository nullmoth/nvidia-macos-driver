#!/usr/bin/env python3
"""Unrelated support kexts must not qualify an incomplete graphics installation."""
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
source = (root/'app/Sources/main.swift').read_text()
start = source.index('    static func loadedDriverComponentCount(')
end = source.index('    static func scanMac(',start)
checks = '''
let all = "com.nullmoth.NVRM (0.1)\\ncom.nullmoth.NVRMFB (0.1)\\ncom.nullmoth.NVAccel (0.1)\\ncom.nullmoth.NVRMAGDC (0.1)"
assert(App.loadedDriverComponentCount(all) == 4)
assert(App.loadedDriverComponentCount(all + "\\n" + all) == 4)
assert(App.loadedDriverComponentCount("com.nullmoth.RTW89Bridge com.nullmoth.RTW89Legacy com.nullmoth.NMDRM com.nullmoth.NMXGPU") == 0)
assert(App.loadedDriverComponentCount("com.nullmoth.NVRMFB com.nullmoth.NVAccel com.nullmoth.NMDRM com.nullmoth.RTW89Bridge") == 2)
assert(App.loadedDriverComponentCount("com.nullmoth.NVRMHelper") == 0)
print("Exact graphics-component qualification passed.")
'''
with tempfile.TemporaryDirectory() as directory:
    script = Path(directory)/'components.swift';exe=Path(directory)/'components'
    script.write_text('import Foundation\nstruct App {\n'+source[start:end]+'}\n'+checks)
    subprocess.run(['xcrun','swiftc',str(script),'-o',str(exe)],check=True,timeout=90)
    subprocess.run([str(exe)],check=True,timeout=30)
