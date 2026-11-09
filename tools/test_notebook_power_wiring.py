#!/usr/bin/env python3
"""The notebook power budget, the platform-power _DSM families, the integrated-GPU panel proxy and the render-encoder
repeat-bind switches are wired into the kexts, the build and the plugin (argument: the tree root, default this one)."""
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
fails = 0


def read(rel):
    p = root / rel
    return p.read_text() if p.exists() else ''


def check(name, ok):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name)
    fails += 0 if ok else 1


nvrm = read('kexts/NVRM/NVRM.cpp')
power = read('kexts/NVRM/nvrm_power.cpp')
build = read('build/build_nvrm.sh')
check('the power manager exists and is read only with boot-arg nvrmtgp', 'PE_parse_boot_argn("nvrmtgp"' in power)
check('the power manager is built into the kext', re.search(r'CXX_SOURCES=\([^)]*nvrm_power\.cpp', build) is not None)
check('the control that needs the GPU object is compiled with the RM command line', 'nvrm_pmgr.c' in build and 'rmcc.py' in build)
check('the kext starts the manager once the GPU is up', 'nvrm_power_start(&fNv)' in nvrm)
check('the kext stops the manager before the adapter goes away', re.search(r'void NVRM::stop\(IOService \*provider\)\s*\{\s*nvrm_power_stop\(\);', nvrm) is not None)
check('a suspended GPU is not sent controls', 'nvrm_power_suspend();' in nvrm)
check('a resume that worked re-arms the manager (the firmware forgot TGP mode)', 'if (ok) nvrm_power_resume();' in nvrm)
pm = read('kexts/NVRM/nvrm_pmgr.c')
check('only the TGP control is reachable (no generic RM control entry)', 'CONFIGURE_TGP_MODE' in pm and 'pParams' not in pm)
check('the control is sent under the API lock and the GPU locks', 'rmapiLockAcquire' in pm and 'rmDeviceGpuLocksAcquire' in pm)

x2 = read('kexts/NVRM/os-xnu2.cpp')
check('the _DSM entry routes through the tested classifier', 'nvrm_dsm_classify(' in x2)
check('the old NBCI-only gate is gone', 'nv_xnu_nbci_guid' not in x2)
check('GPS and NVPCF each sit behind their own boot-arg', '"nvrmgps"' in x2 and '"nvrmnvpcf"' in x2)
check('non-NBCI _DSM calls are refused where AML may not sleep', 'nv_xnu_acpi_may_sleep()' in x2)
check('the notebook answer comes from ACPI when a platform-power boot-arg is set', 'nv_xnu_power_is_notebook' in read('kexts/NVRM/os-xnu.cpp'))

fb = read('kexts/NVRMFB/fb/nvrm-fb.cpp')
check('the proxy head exists behind nvfbigpu', 'PE_parse_boot_argn("nvfbigpu"' in fb and 'bool NVRMNVDAFramebuffer::proxyFind()' in fb)
check('the proxy decision is the tested one', 'nvrm_proxy_decide(' in fb)
for fn in ['enableController()', 'setDisplayMode(', 'validateDetailedTiming(', 'setDetailedTimings(', 'getInformationForDisplayMode(',
           'getPixelInformation(', 'getVRAMRange()', 'getApertureRange(', 'nvrmSetGamma(']:
    i = fb.find('NVRMNVDAFramebuffer::' + fn)
    body = fb[i:i + 2500] if i >= 0 else ''
    check('the proxy answers %s itself' % fn.rstrip('('), 'fProxy' in body)
check('NVKMS flips are refused on the proxy', 'fProxy && functionName' in fb and 'kIOReturnUnsupported' in fb)
check('the proxy publishes a CPU-only scanout (no physical address)', 'r->phys = 0; r->kva = (void *)fPxMap->getVirtualAddress()' in fb)

obj = read('plugin/NVMTLObjects.m')
check('the repeat pipeline skip has its off switch', 'NVMTL_NO_PSO_SKIP' in obj and 'nvmtl_pso_skip(' in obj)
check('the sampler-variant memo has its off switch', 'NVMTL_NO_SV_MEMO' in obj and 'nvmtl_sv_memo_hit(' in obj)
check('a refused sampler variant is remembered by the encoder too', '_svSel=[NSNull null]' in obj)
check('the encoder forgets its choice at the end of encoding', re.search(r'- \(void\)endEncoding \{\s*_cb->_nEnd\+\+;\s*_svFor=nil;', obj) is not None)

sys.exit(1 if fails else 0)
