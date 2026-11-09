#!/bin/bash
# hybrid_switch.sh and hybrid_verify.sh, on the host, against stub nvram and saved logs.
#   bash test_hybrid_scripts.sh      -> 0 failed
cd "$(dirname "$0")" || exit 1
fails=0
arm() { if [ "$2" = 1 ]; then echo "ok   $1"; else echo "FAIL $1"; fails=$((fails+1)); fi; }
T=$(mktemp -d); trap 'rm -rf "$T"' EXIT

# ---- the switch, against a stub nvram that holds boot-args in a file
cat > "$T/nvram" <<'STUB'
#!/bin/bash
F=${NMH_STUB_FILE}
case "$1" in boot-args) printf 'boot-args\t%s\n' "$(cat "$F")";; boot-args=*) printf '%s' "${1#boot-args=}" > "$F";; esac
STUB
chmod +x "$T/nvram"; export NMH_NVRAM="$T/nvram" NMH_STUB_FILE="$T/ba"
sw() { bash ./hybrid_switch.sh "$@"; }
printf '%s' '-v keepsyms=1 nvaccel=1' > "$T/ba"
arm "status with nothing set says off" "$([ "$(sw status)" = "hybrid: off (nmhybrid not set)" ] && echo 1 || echo 0)"
arm "copy adds nmhybrid=1 and keeps the other boot-args" "$([ "$(sw copy --dry-run)" = "-v keepsyms=1 nvaccel=1 nmhybrid=1" ] && echo 1 || echo 0)"
arm "import adds nmhybrid=2" "$([ "$(sw import --dry-run)" = "-v keepsyms=1 nvaccel=1 nmhybrid=2" ] && echo 1 || echo 0)"
printf '%s' 'nmhybrid=1 -v nvaccel=1' > "$T/ba"
arm "import replaces an existing value, once" "$([ "$(sw import --dry-run)" = "-v nvaccel=1 nmhybrid=2" ] && echo 1 || echo 0)"
arm "off removes it and keeps the rest" "$([ "$(sw off --dry-run)" = "-v nvaccel=1" ] && echo 1 || echo 0)"
arm "status reads level 1" "$([ "$(sw status)" = "hybrid: level 1, copy (nmhybrid=1)" ] && echo 1 || echo 0)"
printf '%s' 'nmhybrid=7' > "$T/ba"
arm "status calls an unknown value off, as the driver does" "$(sw status | grep -q 'off (nmhybrid=7 is not 1 or 2)' && echo 1 || echo 0)"
arm "a dry run writes nothing" "$([ "$(cat "$T/ba")" = "nmhybrid=7" ] && echo 1 || echo 0)"
arm "a write without root is refused" "$([ "$(id -u)" = 0 ] || { sw copy >/dev/null 2>&1; [ $? = 1 ] && [ "$(cat "$T/ba")" = "nmhybrid=7" ]; } && echo 1 || echo 0)"
arm "an unknown command is a usage error" "$(sw bogus >/dev/null 2>&1; [ $? = 2 ] && echo 1 || echo 0)"

# ---- the verifier, against saved logs
V() { bash ./hybrid_verify.sh "$@"; }
export NMH_SYSCTL_FILE=hybrid_fixtures/sysctl_copy.txt
out=$(V hybrid_fixtures/copy_ok.log); rc=$?
arm "a copy boot is proven: verdict copy proven" "$([ $rc = 0 ] && echo "$out" | grep -q 'VERDICT  copy proven' && echo 1 || echo 0)"
arm "a copy boot proves the changed-pixels-only step from the counters" "$(echo "$out" | grep -q 'PASS  S4' && echo 1 || echo 0)"
out=$(V hybrid_fixtures/import_ok.log); rc=$?
arm "an import boot is proven: verdict import proven" "$([ $rc = 0 ] && echo "$out" | grep -q 'VERDICT  import proven' && echo 1 || echo 0)"
out=$(V hybrid_fixtures/import_stuck.log); rc=$?
arm "a flip that never landed is not proven" "$([ $rc = 1 ] && echo "$out" | grep -q 'FAIL  S6' && echo "$out" | grep -q 'VERDICT  not proven' && echo 1 || echo 0)"
out=$(V hybrid_fixtures/import_unavailable.log); rc=$?
arm "a generation without import is a proven copy, with the reason shown" "$([ $rc = 0 ] && echo "$out" | grep -q 'import unavailable: display version 14' && echo "$out" | grep -q 'VERDICT  copy proven' && echo 1 || echo 0)"
out=$(V hybrid_fixtures/no_provider.log); rc=$?
arm "a log with no panel driver fails at S2" "$([ $rc = 1 ] && echo "$out" | grep -q 'FAIL  S2' && echo "$out" | grep -q 'VERDICT  not proven: S2' && echo 1 || echo 0)"
out=$(V hybrid_fixtures/resume_ok.log); rc=$?
arm "a wake that relit the panel is proven (R1)" "$([ $rc = 0 ] && echo "$out" | grep -q 'PASS  R1' && echo "$out" | grep -q 'last step the panel driver logged: 13 resume-done' && echo 1 || echo 0)"
out=$(V hybrid_fixtures/resume_failed.log); rc=$?
arm "a wake that did not relight the panel fails R1 and says why in the step trail" "$([ $rc = 1 ] && echo "$out" | grep -q 'FAIL  R1' && echo 1 || echo 0)"
out=$(V hybrid_fixtures/copy_ok.log)
arm "a log without resume armed says nothing about R1" "$(echo "$out" | grep -q 'R1' && echo 0 || echo 1)"
export NMH_SYSCTL_FILE=hybrid_fixtures/sysctl_full.txt
out=$(V hybrid_fixtures/copy_ok.log); rc=$?
arm "every frame stored in full (nothing skipped) fails S4: the changed-pixels path is not working" "$([ $rc = 1 ] && echo "$out" | grep -q 'FAIL  S4' && echo 1 || echo 0)"
unset NMH_SYSCTL_FILE
out=$(V hybrid_fixtures/off.log); rc=$?
arm "an empty log with the switch off says off and is not a failure" "$([ $rc = 0 ] && echo "$out" | grep -q 'VERDICT  off' && echo 1 || echo 0)"

# ---- the log lines the verifier looks for must still be written by the kernel code
SRC="../kexts/NVRM/accel/nvrm-accel.cpp ../kexts/NMIntelFB/intelfb.cpp"
for phrase in 'hybrid: switch level' 'hybrid: provider NMIntelFB' 'hybrid: first frame presented by copy' 'hybrid: import ring' \
              'hybrid: zero-copy flip #1' 'hybrid: resume armed' 'hybrid: resume: ' 'hybrid: standing down' 'hybrid: step ' 'hybrid: import unavailable' 'did not reach the screen' 'hybrid: fallback to copy'; do
    # shellcheck disable=SC2086
    arm "the kernel still logs: $phrase" "$(grep -q -- "$phrase" $SRC && echo 1 || echo 0)"
done
echo "$fails failed"
[ "$fails" = 0 ]
