#!/bin/bash
# Usage: xproc_run.sh <binary>   (binary = xproc_event built for this machine)
# Registers a transient per-user launchd job whose MachServices entry lets the client reach the
# server role, runs the client, then removes the job.
set -u
BIN=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
LABEL=org.nullmoth.xproc-oracle
PL=${TMPDIR:-/tmp}/$LABEL.plist
D=gui/$(id -u)
cat > "$PL" <<P
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>Label</key><string>$LABEL</string>
<key>ProgramArguments</key><array><string>$BIN</string><string>server</string></array>
<key>MachServices</key><dict><key>$LABEL</key><true/></dict>
<key>StandardOutPath</key><string>${TMPDIR:-/tmp}/xproc-server.log</string>
<key>StandardErrorPath</key><string>${TMPDIR:-/tmp}/xproc-server.log</string>
</dict></plist>
P
launchctl bootout $D/$LABEL 2>/dev/null
launchctl bootstrap $D "$PL" || { echo "FAIL could not bootstrap launchd job"; exit 3; }
"$BIN" client; rc=$?
launchctl bootout $D/$LABEL 2>/dev/null
rm -f "$PL"
exit $rc
