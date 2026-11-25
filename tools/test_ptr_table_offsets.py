#!/usr/bin/env python3
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

rustc = shutil.which("rustc")
if rustc is None:
    print("SKIP: pointer table tests require rustc")
    sys.exit(2)

root = Path(__file__).resolve().parents[1]
source = root / "translator/translator/src/native/ptr_table_offsets.rs"
with tempfile.TemporaryDirectory(prefix="nvmtl-ptr-table-") as tmp:
    binary = Path(tmp) / "ptr_table_offsets"
    subprocess.run([rustc, "--edition=2021", "--test", source, "-o", binary], check=True, timeout=60)
    subprocess.run([binary], check=True, timeout=60)
