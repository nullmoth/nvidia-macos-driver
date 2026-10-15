#!/bin/bash
# prove_tahoe_kmutil.sh [kexts-dir]: prove the AMD/Intel/NPU kexts link against Tahoe's (macOS 26) kernel collections.
# It builds an auxiliary kernel collection in a scratch directory from the three kexts against the running system's
# Boot/System kernel collections and then inspects it - exactly the link setup --gpu-test performs, minus the install.
# It writes nothing outside its scratch dir: no /Library/Extensions, no /Library/KernelCollections, no boot-args, no
# NVRAM. Run it on the macOS 26 box. It is a link proof, not an on-GPU test.
#
#   kexts-dir defaults to ./kexts26 (NMXGPU.kext, NMXAccel.kext, NMXNPU.kext built/staged for 26).
#
# WHY inspect and not trust kmutil's exit code: without root:wheel + --repository + --explicit-only, kmutil create
# returns 0 and silently leaves a kext out (measured on macOS 26.7.1). So this asserts each bundle id is IN the result KC.
set -uo pipefail
KDIR=${1:-./kexts26}
MAJ=$(sw_vers -productVersion); MAJ=${MAJ%%.*}
[ "$MAJ" = 26 ] || { echo "FAIL: this proof must run on macOS 26 (Tahoe); this is macOS $(sw_vers -productVersion)"; exit 2; }
KCDIR=/System/Library/KernelCollections
[ -f "$KCDIR/BootKernelExtensions.kc" ] && [ -f "$KCDIR/SystemKernelExtensions.kc" ] || { echo "FAIL: no Tahoe kernel collections at $KCDIR"; exit 2; }

WANT="com.nullmoth.NMXGPU com.nullmoth.NMXAccel com.nullmoth.NMXNPU"
for b in NMXGPU NMXAccel NMXNPU; do
  [ -d "$KDIR/$b.kext" ] || { echo "FAIL: $KDIR/$b.kext is missing"; exit 2; }
done

SCRATCH=$(mktemp -d "${TMPDIR:-/tmp}/nmx-tahoe-proof.XXXXXX") || exit 2
# a private repository the kexts are copied into, owned root:wheel as kmutil requires for an explicit build
REPO="$SCRATCH/repo"; mkdir -p "$REPO"
/bin/cp -R "$KDIR/NMXGPU.kext" "$KDIR/NMXAccel.kext" "$KDIR/NMXNPU.kext" "$REPO/" 2>/dev/null
cp -R "$KDIR"/*.kext "$REPO/" 2>/dev/null   # harmless if the line above already copied them
sudo chown -R root:wheel "$REPO"
K=/System/Library/Kernels/kernel; KA=(--allow-missing-kdk); [ -f "$K" ] && KA+=(--kernel "$K")
AUX="$SCRATCH/aux.kc"
echo "linking $WANT against Tahoe KC into $AUX (scratch only) ..."
sudo kmutil create -n aux --volume-root / "${KA[@]}" \
  -B "$KCDIR/BootKernelExtensions.kc" -S "$KCDIR/SystemKernelExtensions.kc" \
  --repository "$REPO" --explicit-only \
  --bundle-path "$REPO/NMXGPU.kext" --bundle-path "$REPO/NMXAccel.kext" --bundle-path "$REPO/NMXNPU.kext" \
  -A "$AUX" -z > "$SCRATCH/kmutil.log" 2>&1   # --explicit-only builds only the bundles named here
RC=$?
if [ $RC -ne 0 ]; then echo "FAIL: kmutil create rc=$RC"; sed -n '1,40p' "$SCRATCH/kmutil.log"; echo "(scratch: $SCRATCH)"; exit 1; fi

INSPECT=$(sudo kmutil inspect -B "$KCDIR/BootKernelExtensions.kc" -S "$KCDIR/SystemKernelExtensions.kc" -A "$AUX" 2>/dev/null)   # inspect has no -k; an aux KC is read with its boot and system KCs
miss=0
for id in $WANT; do
  if printf '%s\n' "$INSPECT" | grep -q "$id"; then echo "ok   $id links into the Tahoe auxiliary KC"
  else echo "FAIL $id is NOT in the linked KC (unresolved Tahoe KPI symbol - needs a 26-specific build)"; miss=1; fi
done
echo "kmutil log: $SCRATCH/kmutil.log  (remove $SCRATCH when done; nothing was installed)"
[ $miss -eq 0 ] && echo "PASS: all three NMX kexts link against macOS 26" || exit 1
