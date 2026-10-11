#!/bin/bash
# Runs vkprobe against Mesa RADV / ANV on macOS, one simulated GPU at a time, through the drm-shim fake kernel driver.
# Prints one line per chip: device name + whether vkCreateDevice succeeded, or the first error.
cd $NULLMOTH_BUILD_ROOT/build/gpu-src || exit 1
S=$PWD/mesa/build-x86/src
pass=0; fail=0
run() { # kind name env...
  local kind=$1 name=$2; shift 2
  env "$@" ./vkprobe >o.txt 2>&1; local rc=$?
  local line; line=$(grep -m1 "^device" o.txt | sed -E "s/^device 0: //; s/ \| API.*//") 
  if grep -q "vkCreateDevice: OK" o.txt && grep -q "compute pipeline: OK" o.txt; then pass=$((pass+1)); printf "%-4s %-18s OK    %s\n" "$kind" "$name" "$line"
  else fail=$((fail+1)); printf "%-4s %-18s FAIL  rc=%s %s\n" "$kind" "$name" "$rc" "$(grep -m1 -iE "error|fail|assert" o.txt | cut -c1-110)"; fi
}
for g in pitcairn bonaire stoney polaris10 polaris12 vega10 vega20 raven raven2 renoir navi10 navi21 vangogh rembrandt navi31 navi33 phoenix raphael_mendocino strix1 gfx1201; do
  run AMD $g AMDGPU_GPU_ID=$g DYLD_INSERT_LIBRARIES=$S/amd/drm-shim/libamdgpu_noop_drm_shim.dylib VK_DRIVER_FILES=$S/amd/vulkan/radeon_devenv_icd.x86_64.json
done
for g in skl bxt kbl glk cfl whl cml icl ehl jsl tgl rkl dg1 adl rpl dg2 mtl arl lnl bmg ptl; do
  run INTL $g INTEL_STUB_GPU_PLATFORM=$g DYLD_INSERT_LIBRARIES=$S/intel/libintel_noop_drm_shim.dylib VK_DRIVER_FILES=$S/intel/vulkan/intel_devenv_icd.x86_64.json
done
echo "matrix: $pass chip(s) device+pipeline OK, $fail failed"
