#!/bin/bash
# kxld_build.sh <kernel source tag>: build the release's kext linker (libkxld) and the kxlink driver as x86_64
# user programs under $K/bin/<tag>/. Sources come from the published kernel source for that tag.
K=${K:?work folder}; T=${1:?tag}; S=$K/xnu/$T; O=$K/bin/$T; mkdir -p "$K/dl" "$S" "$O"
if [ ! -d "$S/libkern/kxld" ]; then
  [ -f "$K/dl/$T.tgz" ] || curl -fsSL -o "$K/dl/$T.tgz" "https://github.com/apple-oss-distributions/xnu/archive/refs/tags/$T.tar.gz" || { echo "$T: fetch failed"; exit 1; }
  tar -xzf "$K/dl/$T.tgz" -C "$S" --strip-components=1 || { echo "$T: unpack failed"; exit 1; }
fi
cd "$S/libkern/kxld" || exit 1
# two source defects newer compilers reject (a missing comma in an attribute list), and one header newer SDKs lack
grep -l 'nonnull(1,2,4) visibility' *.h 2>/dev/null | while read f; do sed -i '' 's/nonnull(1,2,4) visibility/nonnull(1,2,4), visibility/' "$f"; done
mkdir -p "$O/shim/mach-o/ppc"
[ -f "$O/shim/mach-o/ppc/reloc.h" ] || printf '%s\n' 'enum reloc_type_ppc { PPC_RELOC_VANILLA, PPC_RELOC_PAIR, PPC_RELOC_BR14, PPC_RELOC_BR24, PPC_RELOC_HI16, PPC_RELOC_LO16, PPC_RELOC_HA16, PPC_RELOC_LO14, PPC_RELOC_SECTDIFF, PPC_RELOC_PB_LA_PTR, PPC_RELOC_HI16_SECTDIFF, PPC_RELOC_LO16_SECTDIFF, PPC_RELOC_HA16_SECTDIFF, PPC_RELOC_JBSR, PPC_RELOC_LO14_SECTDIFF, PPC_RELOC_LOCAL_SECTDIFF };' > "$O/shim/mach-o/ppc/reloc.h"
API=3; grep -q 'vm_size_t pagesize' ../libkern/kxld.h || API=2; grep -q 'link_state_size' ../libkern/kxld.h && API=1
SRCS=$(ls kxld*.c | grep -v -e kxld_stubs.c)
xcrun clang -arch x86_64 -mmacosx-version-min=10.13 -std=gnu99 -w -O1 -g -DPRIVATE -DKX_API=$API -I"$O/shim" -D__ASSERT_MACROS_DEFINE_VERSIONS_WITHOUT_UNDERSCORES=1 -I. -I.. -I../libkern -idirafter ../../EXTERNAL_HEADERS \
  $SRCS "$K/kxlink.c" -lc++ -o "$O/kxlink" 2> "$O/build.err"
rc=$?
[ $rc = 0 ] && echo "$T: built $O/kxlink" || { echo "$T: build failed"; head -30 "$O/build.err"; }
exit $rc
