#!/bin/bash
# boost_daemon.sh install|remove|status|set on|off [<pkgroot>]
#
# The opt-in GPU boost daemon (tools/nvmtl-boost): one resource-manager client that holds the system-wide "boost to maximum"
# request while /Library/NullMoth/gpuboost says gpuboost=1, so the boost can be switched at run time (the Mac app's driver
# settings toggle runs "set on" / "set off"). OFF unless the installer is run with NULLMOTH_GPU_BOOST=1, and the daemon is
# not loaded at all until then. install copies the binary from <pkgroot>/Library/NullMoth/boost, writes the setting
# (gpuboost=1: the user asked for it), installs and loads the LaunchDaemon; remove undoes exactly that; set only edits
# the setting and does nothing when the daemon is not installed.
set -u
PATH=/usr/bin:/bin:/usr/sbin:/sbin; export PATH
P=${NULLMOTH_BOOST_ROOT:-}                       # a test points this at a temp folder
BIN="$P/usr/local/libexec/nullmoth/nvmtl-boost"
CONF="$P/Library/NullMoth/gpuboost"
PLIST="$P/Library/LaunchDaemons/com.nullmoth.nvmtl-boost.plist"
LABEL=com.nullmoth.nvmtl-boost
cmd=${1:-status}
launch() { [ -n "${NULLMOTH_BOOST_NO_LAUNCHCTL:-}" ] || launchctl "$@" 2>/dev/null; }
write_conf() {  # root-owned, not writable by others: the daemon refuses any other file
  mkdir -p "$(dirname "$CONF")" || return 1
  printf 'gpuboost=%s\n' "$1" > "$CONF.new" && chmod 644 "$CONF.new" && { [ -n "$P" ] || chown root:wheel "$CONF.new"; } && mv -f "$CONF.new" "$CONF"
}
case "$cmd" in
status)
  if [ -f "$PLIST" ] && [ -x "$BIN" ]; then echo "installed; $(cat "$CONF" 2>/dev/null || echo 'no setting')"; else echo "not installed"; fi ;;
install)
  ROOTDIR=${2:?pkgroot}
  SRC="$ROOTDIR/Library/NullMoth/boost/nvmtl-boost"; SPL="$ROOTDIR/Library/NullMoth/boost/$LABEL.plist"
  [ -f "$SRC" ] && [ -f "$SPL" ] || { echo "gpu boost: this package carries no boost daemon; nothing installed"; exit 0; }
  if [ -z "$P" ]; then lipo -archs "$SRC" 2>/dev/null | grep -qx x86_64 || { echo "gpu boost: the daemon is not an x86_64 binary; nothing installed"; exit 1; }; fi
  mkdir -p "$(dirname "$BIN")" "$(dirname "$PLIST")" || exit 1
  launch bootout "system/$LABEL"
  cp "$SRC" "$BIN.new" && chmod 755 "$BIN.new" && { [ -n "$P" ] || chown root:wheel "$BIN.new"; } && mv -f "$BIN.new" "$BIN" || { rm -f "$BIN.new"; echo "gpu boost: copy failed; nothing left behind"; exit 1; }
  cp "$SPL" "$PLIST.new" && chmod 644 "$PLIST.new" && { [ -n "$P" ] || chown root:wheel "$PLIST.new"; } && mv -f "$PLIST.new" "$PLIST" || { rm -f "$PLIST.new"; exit 1; }
  [ -f "$CONF" ] || write_conf 1 || exit 1
  launch bootstrap system "$PLIST"
  echo "gpu boost: installed (turn off with: sudo $0 set off; remove with: sudo $0 remove)" ;;
set)
  v=${2:-}; case "$v" in on) v=1;; off) v=0;; *) echo "usage: $0 set on|off"; exit 2;; esac
  [ -x "$BIN" ] && [ -f "$PLIST" ] || { echo "gpu boost: not installed"; exit 1; }
  write_conf "$v" || exit 1
  echo "gpu boost: gpuboost=$v (the daemon follows within two seconds)" ;;
remove)
  launch bootout "system/$LABEL"
  rm -f "$PLIST" "$BIN" "$CONF" "$CONF.new"
  rmdir "$(dirname "$BIN")" 2>/dev/null || true
  echo "gpu boost: removed" ;;
*) echo "usage: $0 install <pkgroot> | remove | status | set on|off"; exit 2 ;;
esac
