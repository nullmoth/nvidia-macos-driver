#!/usr/bin/env python3
"""The plugin reports late presents to the integrated GPU's service only when that service asks for them, at a bounded rate
(plugin/nvmtl_late_frames.h, plugin/NVMTLObjects.m). The rate arms are in test_late_frames.c; this checks the wiring."""
import pathlib, sys
ROOT = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1]
bad = 0
def check(name, ok):
    global bad
    bad += not ok
    print(("ok   " if ok else "FAIL ") + name)
p = ROOT / "plugin/NVMTLObjects.m"
s = p.read_text()
check("a present is judged by the tested detector", "nvmtl_late_note(" in s and "#include \"nvmtl_late_frames.h\"" in s)
check("reporting starts only if the GPU service publishes nullmoth-floor-percent", 'CFSTR("nullmoth-floor-percent")' in s)
check("the report is the nullmoth-frame-late property on the NMXGPU service", 'IOServiceMatching("NMXGPU")' in s and 'CFSTR("nullmoth-frame-late")' in s)
check("a refused report switches reporting off for the process", "state = 0; nvlog(\"late-frame report refused" in s)
check("it runs on the present path", s.index("nvmtl_late_present();") > s.index("- (void)nvmtlRecordDrawable:"))
sys.exit(1 if bad else 0)
