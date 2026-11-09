#!/bin/bash
# nm-dmc-load.sh [--dest DIR]
#
# Root helper for the hybrid-laptop panel driver: finds out which Intel display firmware file this GPU needs
# (debug.nmintelfb_dmc_want), makes sure the verified file is on disk (package/dmc_fetch.sh), checks its SHA-256 again, and hands
# the bytes to the driver (package/nm_dmc_push). The driver parses and range-checks them before keeping anything. Nothing happens
# on a machine without the panel driver or whose GPU needs no file. Run after the panel driver has started, once per boot.
set -u
PATH=/usr/bin:/bin:/usr/sbin:/sbin; export PATH
HERE=$(cd "$(dirname "$0")" && pwd)
. "$HERE/dmc_pins.sh" || exit 2
if [ "$(id -u)" != 0 ] && [ -n "${DMC_TEST_PINS:-}" ]; then . "$DMC_TEST_PINS" || exit 2; fi   # tests only, never as root
DEST=/Library/GPUBundles/nvmtl
[ "${1:-}" = --dest ] && DEST=${2:-$DEST}
[ "$(id -u)" = 0 ] || [ -n "${NM_DMC_TEST:-}" ] || { echo "nm-dmc-load: needs root" >&2; exit 1; }
WANT=$(${NM_DMC_SYSCTL:-sysctl} -n debug.nmintelfb_dmc_want 2>/dev/null) || { echo "nm-dmc-load: the panel driver is not running"; exit 0; }
[ -n "$WANT" ] && [ "$WANT" != none ] || { echo "nm-dmc-load: this GPU needs no display firmware file"; exit 0; }
case " $FILES " in *" $WANT "*) ;; *) echo "nm-dmc-load: unknown file name from the driver: $WANT" >&2; exit 1;; esac
F="$DEST/i915/$WANT"
if [ ! -f "$F" ] || [ "$(/usr/bin/shasum -a 256 "$F" | cut -d' ' -f1)" != "$(dmc_sha "$WANT")" ]; then
  bash "$HERE/dmc_fetch.sh" --dest "$DEST" || exit 1
fi
[ "$(/usr/bin/shasum -a 256 "$F" | cut -d' ' -f1)" = "$(dmc_sha "$WANT")" ] || { echo "nm-dmc-load: $F does not match its pin" >&2; exit 1; }
exec "${NM_DMC_PUSH:-$HERE/nm_dmc_push}" "$F"
