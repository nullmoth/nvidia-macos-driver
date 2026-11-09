#!/bin/bash
# test_claims.sh [out dir]: the claim invariant, gated (all arms pass) and ungated (the GCN arm must fail).
set -u
H=$(cd "$(dirname "$0")" && pwd); O=${1:-$(mktemp -d)}; CC=${CC:-cc}
$CC -std=c11 -Wall -o "$O/claims" "$H/test_claims.c" || { echo "BUILD FAILED"; exit 2; }
$CC -std=c11 -Wall -DNVMTL_CLAIMS_UNGATED -o "$O/claims_ungated" "$H/test_claims.c" || { echo "BUILD FAILED (ungated)"; exit 2; }
"$O/claims"; a=$?
echo "-- negative control (ungated claims):"
"$O/claims_ungated"; b=$?
if [ $a -eq 0 ] && [ $b -ne 0 ]; then echo "claims: PASS (gated passes, ungated fails as it must)"; exit 0; fi
echo "claims: FAIL (gated rc=$a, ungated rc=$b)"; exit 1
