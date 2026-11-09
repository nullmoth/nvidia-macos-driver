#!/bin/bash
# boost_daemon.sh install|remove|status|set on|off [<pkgroot>]
#
# The opt-in GPU boost daemon (tools/nvmtl-boost): one resource-manager client that holds the system-wide "boost to maximum"
# request while /Library/NullMoth/gpuboost says gpuboost=1, so the boost can be switched at run time (the Mac app's driver
# settings toggle runs "set on" / "set off"). OFF unless the installer is run with NULLMOTH_GPU_BOOST=1, and the daemon is
# not loaded at all until then. install copies the binary from <pkgroot>/Library/NullMoth/boost to
# /Library/NullMoth/boost/nvmtl-boost, writes the setting (gpuboost=1: the user asked for it), writes the LaunchDaemon plist
# itself (it names only that path) and loads it; remove undoes exactly that; set only edits the setting and does nothing
# when the daemon is not installed.
#
# The daemon runs as root, so every path it is installed to or read from is checked first (safe_chain): each component from
# the root down must exist, must not be a symbolic link, must be owned by root and must not be writable by group or others.
# A path that fails is refused and nothing is changed. install, set and status all check. The binary is copied to a temporary
# file inside the checked folder and renamed into place, so it is never half written and never a link someone else made.
# (/usr/local is not used: on an Intel Mac it is commonly writable by the user.)
set -u
PATH=/usr/bin:/bin:/usr/sbin:/sbin; export PATH
P=${NULLMOTH_BOOST_ROOT:-}                       # a test points this at a temp folder that stands for /
OWNER=${NULLMOTH_BOOST_OWNER_UID:-0}             # the uid that must own every component (a test uses its own, or another)
DIR="$P/Library/NullMoth/boost"
BIN="$DIR/nvmtl-boost"
CONF="$P/Library/NullMoth/gpuboost"
PLIST="$P/Library/LaunchDaemons/com.nullmoth.nvmtl-boost.plist"
LABEL=com.nullmoth.nvmtl-boost
SYSBIN=/Library/NullMoth/boost/nvmtl-boost       # what the plist names, whatever $P is
LEGACY="$P/usr/local/libexec/nullmoth/nvmtl-boost"   # an earlier build put the binary here; it is removed, never run
cmd=${1:-status}
launch() { [ -n "${NULLMOTH_BOOST_NO_LAUNCHCTL:-}" ] || launchctl "$@" 2>/dev/null; }

check_one() {   # one component: present, not a link, owned by $OWNER, not writable by group or others
  local mode shown=${1#"$P"}
  [ -n "$shown" ] || shown=/
  [ -e "$1" ] || { echo "gpu boost: $shown does not exist; refused"; return 1; }
  [ ! -L "$1" ] || { echo "gpu boost: $shown is a symbolic link; refused"; return 1; }
  [ "$(stat -f %u "$1")" = "$OWNER" ] || { echo "gpu boost: $shown is not owned by root; refused"; return 1; }
  mode=$(stat -f %Lp "$1")
  [ $(( 0$mode & 022 )) -eq 0 ] || { echo "gpu boost: $shown can be written by others; refused"; return 1; }
}
safe_chain() {  # every component from the root down to $1 (a path under $P)
  local rel=${1#"$P"} cur=$P part
  check_one "${P:-/}" || return 1
  rel=${rel#/}
  local IFS=/
  for part in $rel; do
    cur="$cur/$part"
    check_one "$cur" || return 1
  done
}
make_dir() {    # create a missing folder as 0755 (the checks run on it afterwards)
  [ -e "$1" ] || mkdir -m 755 -p "$1"
}
put_file() {    # put_file <source> <dest> <mode>: temporary file in the (checked) folder, then an atomic rename
  local tmp
  tmp=$(mktemp "$(dirname "$2")/.nullmoth-boost.XXXXXX") || return 1
  if cp "$1" "$tmp" && chmod "$3" "$tmp" && { [ -n "$P" ] || chown root:wheel "$tmp"; } && mv -f "$tmp" "$2"; then return 0; fi
  rm -f "$tmp"; return 1
}
write_conf() {  # root-owned, not writable by others: the daemon refuses any other file
  local tmp
  safe_chain "$(dirname "$CONF")" || return 1
  tmp=$(mktemp "$(dirname "$CONF")/.nullmoth-boost.XXXXXX") || return 1
  if printf 'gpuboost=%s\n' "$1" > "$tmp" && chmod 644 "$tmp" && { [ -n "$P" ] || chown root:wheel "$tmp"; } && mv -f "$tmp" "$CONF"; then return 0; fi
  rm -f "$tmp"; return 1
}
write_plist() { # the plist names only SYSBIN and is written here, not copied from the package
  local tmp
  tmp=$(mktemp "$(dirname "$PLIST")/.nullmoth-boost.XXXXXX") || return 1
  { cat <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>$LABEL</string>
    <key>ProgramArguments</key><array><string>$SYSBIN</string></array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
</dict>
</plist>
PL
  } > "$tmp" && chmod 644 "$tmp" && { [ -n "$P" ] || chown root:wheel "$tmp"; } && mv -f "$tmp" "$PLIST" || { rm -f "$tmp"; return 1; }
}
installed_ok() { safe_chain "$BIN" && safe_chain "$PLIST"; }

case "$cmd" in
status)
  if [ -f "$PLIST" ] || [ -e "$BIN" ]; then
    installed_ok >/dev/null || { echo "installed, but its path is not trusted (run remove, then install again)"; exit 1; }
    echo "installed; $(cat "$CONF" 2>/dev/null || echo 'no setting')"
  else echo "not installed"; fi ;;
install)
  ROOTDIR=${2:?pkgroot}
  SRC="$ROOTDIR/Library/NullMoth/boost/nvmtl-boost"
  [ -f "$SRC" ] || { echo "gpu boost: this package carries no boost daemon; nothing installed"; exit 0; }
  if [ -z "$P" ]; then lipo -archs "$SRC" 2>/dev/null | grep -qx x86_64 || { echo "gpu boost: the daemon is not an x86_64 binary; nothing installed"; exit 1; }; fi
  make_dir "$P/Library/NullMoth" && make_dir "$DIR" || exit 1
  safe_chain "$DIR" || exit 1
  safe_chain "$(dirname "$PLIST")" || exit 1
  launch bootout "system/$LABEL"
  put_file "$SRC" "$BIN" 755 || { echo "gpu boost: copy failed; nothing left behind"; exit 1; }
  safe_chain "$BIN" || { rm -f "$BIN"; exit 1; }
  write_plist || exit 1
  safe_chain "$PLIST" || { rm -f "$PLIST"; exit 1; }
  [ -f "$CONF" ] || write_conf 1 || exit 1
  rm -f "$LEGACY"
  launch bootstrap system "$PLIST"
  echo "gpu boost: installed (turn off with: sudo $0 set off; remove with: sudo $0 remove)" ;;
set)
  v=${2:-}; case "$v" in on) v=1;; off) v=0;; *) echo "usage: $0 set on|off"; exit 2;; esac
  [ -f "$BIN" ] && [ -f "$PLIST" ] || { echo "gpu boost: not installed"; exit 1; }
  installed_ok || exit 1
  write_conf "$v" || exit 1
  echo "gpu boost: gpuboost=$v (the daemon follows within two seconds)" ;;
remove)
  launch bootout "system/$LABEL"
  rm -f "$PLIST" "$BIN" "$CONF" "$LEGACY"
  rmdir "$DIR" 2>/dev/null || true
  echo "gpu boost: removed" ;;
*) echo "usage: $0 install <pkgroot> | remove | status | set on|off"; exit 2 ;;
esac
