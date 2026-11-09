#!/usr/bin/env python3
"""Wiring checks for nvmultigpu that no host test can run: each arm is a sentence about where a rule sits in the source.
Exit status 0 only when every arm holds."""
import sys
from pathlib import Path
R = Path(__file__).resolve().parents[1]
dev = (R / "plugin/NVMTLDevice.m").read_text(); acc = (R / "kexts/NVRM/accel/nvrm-accel.cpp").read_text()
nvrm = (R / "kexts/NVRM/NVRM.cpp").read_text(); vk = (R / "plugin/nvmtl_vk.c").read_text()
nvkp = (R / "nvk/nvk-card-select.patch").read_text(); bld = (R / "build/build_nvk.sh").read_text()
fails = 0
def arm(w, ok):
    global fails
    print(("PASS  " if ok else "FAIL  ") + w); fails += 0 if ok else 1
i = dev.index("- (instancetype)initWithAcceleratorPort:", dev.index("@implementation NVMTLDevice"))
body = dev[i:i + 2500]
arm("the plugin decides whether this accelerator is its card before it creates the device", body.index("nvmtl_card_wanted(port)") < body.index("[super initWithAcceleratorPort:port]"))
j = dev.index("static BOOL nvmtl_card_wanted")
arm("without nvmultigpu the decision is a plain yes (today's behaviour)", "if (!nvmtl_multi_gpu_on() ||" in dev[j:j + 300] and "return YES" in dev[j:j + 300])
arm("the chosen card is handed to NVK through NVK_ACCEL_ID and to the plugin's own lookups", 'setenv("NVK_ACCEL_ID"' in dev and "nvmtl_card_registry_id" in vk)
arm("the plugin's accelerator lookup uses the bound card only when one is bound", "nvmtl_card_registry_id ?" in vk)
arm("the kernel publishes NVAccelPrimary only under nvmultigpu", "if (!nvaccelMultiGpu()) return;" in acc[acc.index("static void publishPrimary"):][:200] and 'setProperty("NVAccelPrimary"' in acc)
arm("NVK refuses to fall back to another card when NVK_ACCEL_ID names none", "names no NVAccel" in nvkp and "(want_id && *want_id) ? 0" in nvkp)
arm("both gates re-offer refused cards through registerService", "registerService(kIOServiceAsynchronous)" in nvrm and "registerService(kIOServiceAsynchronous)" in acc)
_chain = next((l for l in bld.splitlines() if l.strip().startswith('CHAIN="nvk-macos') and "nvk-card-select" in l), "")
arm("the card-select patch is in the 2.9 chain, right after the Metal surface patch", "nvk-metal-wsi nvk-card-select" in _chain)
print("FAILED %d" % fails if fails else "PASS all arms"); sys.exit(1 if fails else 0)
