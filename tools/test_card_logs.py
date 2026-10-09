#!/usr/bin/env python3
"""Exercise the production log collector and redactor with isolated fixtures."""
import os
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PCI = '''+-o display@0
  "vendor-id" = <de100000>
  "device-id" = <82280000>
  "subsystem-vendor-id" = <de100000>
  "subsystem-id" = <00000000>
  "assigned-addresses" = <fixture-bar-data>
  "nvrm-boot-hold" = "display armed"
+-o unrelated@1
  "vendor-id" = <02100000>
  "device-id" = <ffff0000>
'''
REDACTION = r'''
import Foundation
func sh(_ path: String, _ args: [String]) -> String { return "" }
func services(_ name: String) -> [Int] { return [] }
func str(_ value: Int, _ key: String) -> String? { return nil }
func IOObjectRelease(_ value: Int) {}
PRODUCTION
let path = URL(fileURLWithPath: "/Users").appendingPathComponent("fixture").appendingPathComponent("test.log").path
let sample = path + " GPU 10de:2882 BAR1 0x10000000000"
let result = redact(sample)
assert(result == "[home]/test.log GPU 10de:2882 BAR1 0x10000000000")
assert(redact(NSHomeDirectory() + "/test.log") == "[home]/test.log")
print("PASS home paths removed while PCI IDs and BAR addresses remain")
'''
with tempfile.TemporaryDirectory(prefix="nullmoth-card-log-tests-") as directory:
    tmp = Path(directory)
    commands = tmp / "bin"
    commands.mkdir()
    mocks = {
        "ioreg": "cat <<'PCI'\n" + PCI + "PCI\n",
        "log": "echo 'fixture log query unavailable'; exit 3\n",
        "dmesg": "echo 'NVRM: GSP ring fixture'; echo 'apfs: DONE reverting to snapshot fixture'\n",
        "system_profiler": "echo 'Chipset Model: NVIDIA fixture'\n",
        "sw_vers": "echo 15.8.1\n",
        "sysctl": "echo 'debug.nvaccelfb: 1'\n",
        "kmutil": "echo 'com.nullmoth.NVRM'\n",
        "nvram": "echo 'boot-args fixture'\n",
        "csrutil": "echo 'fixture'\n",
        "stat": "echo root\n",
        "ls": "exit 0\n",
        "sudo": 'echo "$*" >> "$(dirname "$0")/sudo-calls"; shift 2; exec "$@"\n',
    }
    for name, body in mocks.items():
        path = commands / name
        path.write_text("#!/bin/bash\n" + body)
        path.chmod(0o755)
    source = (ROOT / "app/Resources/nullmoth-setup.sh").read_text()
    start = '  { echo "macOS $(sw_vers -productVersion)'
    stop = '  n=0; collection_errors=0'
    assert source.count(start) == source.count(stop) == 1
    body = start + source.split(start, 1)[1].split(stop, 1)[0]
    # Redirect the optional file-log source into the isolated fixture directory.
    assert body.count('/private/tmp/nvmtl.log') == 1
    filelog = tmp / "nvmtl.log"
    filelog.write_text("NVMTL: vkCreateDevice -> -8\n")
    body = body.replace('/private/tmp/nvmtl.log', str(filelog))
    collection = tmp / "logs"
    collection.mkdir()
    script = tmp / "collect.sh"
    script.write_text('COLLECT="$1"\n' + body)
    env = dict(os.environ, PATH=str(commands) + ":" + os.environ["PATH"])
    subprocess.run(["/bin/bash", str(script), str(collection)], env=env, check=True)
    state = (collection / "driver-state.txt").read_text()
    for needle in ('"device-id" = <82280000>', 'fixture-bar-data', '"nvrm-boot-hold" = "display armed"'):
        assert needle in state
    assert 'unrelated@1' not in state and 'ffff0000' not in state
    kernel = (collection / "driver-kernel-log.txt").read_text()
    plugin = (collection / "driver-plugin-log.txt").read_text()
    assert "log show exit: 3" in kernel and "GSP ring fixture" in kernel
    assert "log show exit: 3" in plugin and "vkCreateDevice -> -8" in plugin
    assert "-u nobody tail -c 262144 " + str(filelog) in (commands / "sudo-calls").read_text()
    update = (collection / "driver-update-log.txt").read_text()
    assert "log show exit: 3" in update and "apfs: DONE reverting to snapshot fixture" in update
    print("PASS NVIDIA identifiers, BARs, query failures, kernel ring, plugin errors, and update context retained")
    start, stop = 'collect_recent_logs() {', 'if [ -n "$COLLECT" ]; then'
    assert source.count(start) == source.count(stop) == 1
    helper = start + source.split(start, 1)[1].split(stop, 1)[0]
    volume = tmp / "NO NAME"
    volume.mkdir()
    bootlogs = []
    for i in range(4):
        path = volume / f"opencore-{i}.txt"
        path.write_text(f"boot-{i}\n")
        os.utime(path, (1000 + i, 1000 + i))
        bootlogs.append(path)
    crash = volume / "WindowServer-fixture.ips"
    crash.write_text('{"procName":"WindowServer","threads":[{"frames":[{"symbol":"CFRunLoopRun"}]}]}')
    copied = tmp / "copied"
    copied.mkdir()
    fixture = tmp / "recent.sh"
    fixture.write_text(helper + '\nCOLLECT="$1"; volume="$2"; n=0; collection_errors=0\n'
                       + 'note() { echo "NOTE $*"; }\n'
                       + 'collect_recent_logs disk9s1 3 "$volume"/opencore-*.txt\n'
                       + 'collect_recent_logs macos 3 "$volume"/WindowServer*.ips\n'
                       + 'collect_recent_logs absent 3 "$volume"/missing-*.txt\n'
                       + '[ "$n" = 4 ] && [ "$collection_errors" = 0 ]\n')
    subprocess.run(["/bin/bash", str(fixture), str(copied), str(volume)], check=True)
    assert {p.name for p in copied.iterdir()} == {
        "disk9s1-opencore-1.txt", "disk9s1-opencore-2.txt", "disk9s1-opencore-3.txt",
        "macos-WindowServer-fixture.ips"}
    assert (copied / "macos-WindowServer-fixture.ips").read_text() == crash.read_text()
    print("PASS newest boot logs on NO NAME and full WindowServer report without driver frames")
    failure = tmp / "copy-failure.sh"
    failure.write_text(helper + '\nCOLLECT="$1"; n=0; collection_errors=0\n'
                       + 'note() { echo "NOTE $*"; }\n'
                       + 'collect_recent_logs macos 3 "$2"\n'
                       + '[ "$n" = 0 ] && [ "$collection_errors" = 1 ]\n')
    subprocess.run(["/bin/bash", str(failure), str(tmp / "missing-destination"), str(crash)], check=True)
    # The old production loop must lose the same existing log on the same volume.
    old = 'mp="$1"; COLLECT="$2"; n=0; for f in $(ls -t "$mp"/opencore-*.txt 2>/dev/null | head -3); do cp "$f" "$COLLECT/$(basename "$f")" 2>/dev/null && n=$((n+1)); done; [ "$n" = 0 ]'
    subprocess.run(["/bin/bash", "-c", old, "fixture", str(volume), str(copied)], check=True)
    print("PASS failed copies recorded and original whitespace bug reproduced")
    # Exercise the production collection tail, including its unfiltered crash glob,
    # EFI mount path, newest-file limits, and RESULT partial on copy failure.
    start, stop = '  n=0; collection_errors=0', '\nfi\n\nif [ $REMOVE = 1 ]; then'
    assert source.count(start) == source.count(stop) == 1
    tail = start + source.split(start, 1)[1].split(stop, 1)[0]
    tail = tail.replace('/Library/Logs/DiagnosticReports', '"' + str(volume) + '"')
    full = tmp / "collection-tail.sh"
    full.write_text(helper + '\nCOLLECT="$1"; volume="$2"\n'
                    + 'note() { echo "NOTE $*"; }; ok() { echo "OK $*"; }; cleanup() { :; }\n'
                    + 'diskutil() { echo "1: EFI fixture disk9s1"; }\n'
                    + 'mount_efi() { printf "%s\\n" "$volume"; }\n' + tail)
    for path in copied.iterdir():
        path.unlink()
    result = subprocess.run(["/bin/bash", str(full), str(copied), str(volume)], capture_output=True, text=True)
    assert result.returncode == 0 and 'RESULT ok' in result.stdout, result
    assert 'macos-WindowServer-fixture.ips' in {p.name for p in copied.iterdir()}
    result = subprocess.run(["/bin/bash", str(full), str(tmp / "absent"), str(volume)], capture_output=True, text=True)
    assert result.returncode == 1 and 'RESULT partial' in result.stdout, result
    print("PASS production collection tail includes WindowServer and reports partial collection")
    swift = (ROOT / "app/Sources/main.swift").read_text()
    start, stop = 'func redact(_ s: String) -> String {', 'func hardwareFacts() -> String {'
    assert swift.count(start) == swift.count(stop) == 1
    redact = start + swift.split(start, 1)[1].split(stop, 1)[0]
    fixture = tmp / "redact.swift"
    fixture.write_text(REDACTION.replace("PRODUCTION", (ROOT / "app/Sources/redaction.swift").read_text() + redact))
    subprocess.run(["xcrun", "swift", str(fixture)], check=True)
print("PASS card-log collection regressions")
