#!/bin/bash
set -u
# runs as root: tools resolve through a fixed PATH, never one the caller exported
PATH=/usr/bin:/bin:/usr/sbin:/sbin; export PATH
HERE=$(cd "$(dirname "$0")" && pwd)
INTEL_PANEL_CHOICE=ask
AUDIO_CHOICE=ask
DLSS_CHOICE=ask
WEBKIT_CHOICE=ask
EFI_TARGET=""
[ "${NULLMOTH_DLSS:-0}" != 1 ] || DLSS_CHOICE=yes
while [ "$#" != 0 ]; do
  case "$1" in
    --payload)
      [ "$#" -ge 2 ] && [ -d "$2" ] || { echo "STOP: --payload 后必须是已存在的软件包目录" >&2; exit 2; }
      HERE=$(cd "$2" && pwd) || exit 2; shift 2;;
    --dlss=yes) DLSS_CHOICE=yes; shift;;
    --dlss=no) DLSS_CHOICE=no; shift;;
    --audio=yes) AUDIO_CHOICE=yes; shift;;
    --audio=no) AUDIO_CHOICE=no; shift;;
    --intel-panel=yes) INTEL_PANEL_CHOICE=yes; shift;;
    --intel-panel=no) INTEL_PANEL_CHOICE=no; shift;;
    --webkit-fixup=manual|--webkit-fixup=auto|--webkit-fixup=skip) WEBKIT_CHOICE=${1#*=}; shift;;
    --efi-target)
      [ "$#" -ge 2 ] && [ -n "$2" ] || { echo "STOP: --efi-target 后必须是 diskXsY 或已挂载的 EFI/OC 路径" >&2; exit 2; }
      EFI_TARGET=$2; shift 2;;
    *) echo "STOP: 参数无效。可用参数：--payload 目录、--intel-panel=yes|no、--audio=yes|no、--dlss=yes|no、--webkit-fixup=manual|auto|skip 或 --efi-target 目标" >&2; exit 2;;
  esac
done
KEXTS="NVRM NVAccel NVRMFB NVRMAGDC"
SOURCE_PACKAGE=$HERE
REMOVED_KEXTS=""
EXT=/Library/Extensions; GB=/Library/GPUBundles; FW=/Library/NullMoth/nvfw; FWOLD=/Users/Shared/nvfw
AUDIO_MARKER=/Library/NullMoth/nvidia-audio.sha256
# Only a root-owned record matching the installed binary grants this installer
# ownership. An independently installed or subsequently replaced audio driver
# remains external. Keep this helper identical in install.sh and uninstall.sh.
audio_managed() {
  local p meta owner mode expected actual
  [ -f "$AUDIO_MARKER" ] && [ -d "$EXT/NMXAudio.kext" ] || return 1
  for p in /Library/NullMoth "$AUDIO_MARKER" "$EXT/NMXAudio.kext"; do
    [ ! -L "$p" ] || return 1
    meta=$(stat -f '%u %Lp' "$p" 2>/dev/null) || return 1
    owner=${meta%% *}; mode=${meta#* }
    [ "$owner" = 0 ] || return 1
    case "$mode" in *[2367]?|*[2367]) return 1;; esac
  done
  p="$EXT/NMXAudio.kext/Contents/MacOS/NMXAudio"
  [ -f "$p" ] && [ ! -L "$p" ] || return 1
  expected=$(cat "$AUDIO_MARKER") || return 1
  [ "${#expected}" = 64 ] || return 1
  case "$expected" in *[!0-9a-f]*) return 1;; esac
  actual=$(shasum -a 256 "$p" | awk '{print $1}') || return 1
  [ "$expected" = "$actual" ]
}
KC=/Library/KernelCollections/AuxiliaryKernelExtensions.kc
K=/System/Library/Kernels/kernel
# macOS 13+ kmutil refuses to build any kernel collection without a Kernel Debug Kit matching the build unless it is told
# --allow-missing-kdk. The auxiliary collection only links against the boot and system collections already on disk.
KARG=(--allow-missing-kdk); [ -f "$K" ] && KARG+=(--kernel "$K")
KB=/System/Library/KernelCollections/BootKernelExtensions.kc
KS=/System/Library/KernelCollections/SystemKernelExtensions.kc
BK=""; LOCKDIR=/Library/NullMoth/install.lock; LOCK_HELD=0; LOCK_RELEASE_OK=1; LOCK_ID=""
INSTALLING=0; NEWKC=""; STAGE=""
release_install_lock() {
  if [ "$LOCK_HELD" = 1 ] && [ "$LOCK_RELEASE_OK" = 1 ]; then
    if [ -z "$LOCK_ID" ] || [ -L "$LOCKDIR" ] || [ ! -d "$LOCKDIR" ] ||
       [ "$(stat -f '%d:%i' "$LOCKDIR" 2>/dev/null)" != "$LOCK_ID" ]; then
      echo "   NOTE: 安装保护标记的身份已变化，保留当前路径供检查。" >&2
      return
    fi
    rm -f "$LOCKDIR/owner" "$LOCKDIR/installing"
    rmdir "$LOCKDIR" || echo "   NOTE: 无法释放安装锁；再次安装前请检查。" >&2
    LOCK_HELD=0
  elif [ "$LOCK_HELD" = 1 ]; then
    echo "   NOTE: 由于恢复未完成，保留了安装保护标记；需要检查恢复状态。" >&2
  fi
}

rollback_install() {
  local failed=0 k b restore_kexts="$KEXTS $REMOVED_KEXTS"
  # A later process recovering a stale guard has not detected PRIMARY yet.
  if [ -f "$BK/primary-selector-used" ]; then
    case " $restore_kexts " in *' NVPrimarySelector '*) ;; *) restore_kexts="$restore_kexts NVPrimarySelector";; esac
  fi
  if [ -f "$BK/intel-panel-touched" ]; then
    case " $restore_kexts " in *' NMIntelFB '*) ;; *) restore_kexts="$restore_kexts NMIntelFB";; esac
  fi
  if [ -f "$BK/audio-touched" ]; then
    case " $restore_kexts " in *' NMXAudio '*) ;; *) restore_kexts="$restore_kexts NMXAudio";; esac
  fi
  [ -z "$NEWKC" ] || rm -f "$NEWKC" || failed=1
  for k in $restore_kexts; do
    rm -rf "$EXT/$k.kext" || failed=1
    [ ! -e "$BK/$k.kext" ] || ditto "$BK/$k.kext" "$EXT/$k.kext" || failed=1
  done
  for b in NVMTLDriver.bundle NVIDIAShared.bundle nvmtl NVKZinkGLDriver.bundle nvmtl-allow.txt; do
    rm -rf "$GB/$b" || failed=1
    [ ! -e "$BK/$b" ] || ditto "$BK/$b" "$GB/$b" || failed=1
  done
  rm -rf /Library/NullMoth/kexts "$FW" || failed=1
  [ ! -d "$BK/kexts" ] || ditto "$BK/kexts" /Library/NullMoth/kexts || failed=1
  # Restore the firmware to both locations: a driver older than this one looks for it in /Users/Shared.
  # /Users/Shared is writable by every account, so the old location is written only when root creates it here:
  # mkdir -p adopted a folder or symlink another account had put there and ditto then wrote through it as root.
  # mkdir without -p fails on anything that exists, and a root-owned folder in the sticky parent cannot be swapped.
  [ ! -d "$BK/nvfw" ] || ditto "$BK/nvfw" "$FW" || failed=1
  [ ! -d "$BK/nvfw" ] || ! mkdir -m 0755 "$FWOLD" 2>/dev/null || ditto "$BK/nvfw" "$FWOLD" || failed=1
  if [ -f "$BK/AuxiliaryKernelExtensions.kc" ]; then cp -p "$BK/AuxiliaryKernelExtensions.kc" "$KC" || failed=1
  else rm -f "$KC" || failed=1; fi
  if [ -f "$BK/os-major" ]; then cp -p "$BK/os-major" /Library/NullMoth/os-major || failed=1
  else rm -f /Library/NullMoth/os-major || failed=1; fi
  # the version record follows the files it describes, or Send logs names a version that is not installed
  if [ -f "$BK/driver-version" ]; then cp -p "$BK/driver-version" /Library/NullMoth/driver-version || failed=1
  else rm -f /Library/NullMoth/driver-version || failed=1; fi
  if [ -f "$BK/audio-touched" ]; then
    rm -f "$AUDIO_MARKER" || failed=1
    [ ! -f "$BK/nvidia-audio.sha256" ] || cp -p "$BK/nvidia-audio.sha256" "$AUDIO_MARKER" || failed=1
  fi
  return "$failed"
}
step() { echo; echo "== $*"; }; ok() { echo "   ok  $*"; }
boot_sec() { sysctl -n kern.boottime 2>/dev/null | sed -E 's/.*sec = ([0-9]+),.*/\1/'; }
# kern.boottime is derived from the clock and moves when the clock is corrected after boot, so "the same boot" allows
# 60 s of drift (review: an exact match could take a live install for a dead one and roll its files back mid-install).
same_boot() { local a=$1 b; b=$(boot_sec); [ -n "$a" ] && [ -n "$b" ] && [ $((a > b ? a - b : b - a)) -le 60 ]; }
# the pid is still the installer (pid numbers are reused): its command line names this script
is_installer_pid() { ps -o command= -p "$1" 2>/dev/null | grep -Eq '(nullmoth-install|/install)\.sh'; }
# A guard left by an install that was killed outright (power loss, a forced quit, a restart mid-install) stopped every
# later install with "installer guard already exists" until someone removed it by hand (3 reports and chat, 10-12).
# The guard now records its owner (pid, boot) and, once files start changing, the backup it can be restored from.
# A guard whose owner is gone is recovered: rolled back from that backup if it had started changing files, then cleared.
# A live install, or one whose own restore failed (restore-incomplete), still stops.
recover_stale_guard() {
  local g=$LOCKDIR o=$LOCKDIR/owner pid="" boot="" bk=""
  [ -d "$g" ] && [ ! -L "$g" ] && [ "$(stat -f '%u' "$g" 2>/dev/null)" = 0 ] || return 0
  [ ! -e "$g/restore-incomplete" ] || die "上次安装未能恢复旧驱动（备份保存在 /Library/NullMoth）；请先运行 Remove NVIDIA driver，再重新安装，或发送日志（未更改驱动）"
  if [ -f "$o" ] && [ ! -L "$o" ]; then
    pid=$(sed -n 's/^pid=\([0-9][0-9]*\)$/\1/p' "$o" | head -1); boot=$(sed -n 's/^boot=\([0-9][0-9]*\)$/\1/p' "$o" | head -1)
    bk=$(sed -n 's/^bk=\(.*\)$/\1/p' "$o" | head -1)
    if [ -n "$pid" ] && same_boot "$boot" && kill -0 "$pid" 2>/dev/null && is_installer_pid "$pid"; then return 0; fi
  else
    # a guard from 1.14.7 or earlier has no owner record: an install cannot outlive a restart, so one older than this boot is stale
    [ "$(stat -f '%m' "$g" 2>/dev/null || echo 0)" -lt $(( $(boot_sec) - 60 )) ] || return 0
  fi
  echo "   NOTE: 检测到先前安装中断后遗留的保护标记，正在恢复。"
  if [ -e "$g/installing" ]; then
    case "$bk" in /Library/NullMoth/backup-*) ;; *) die "中断的安装已更改驱动文件，但未记录备份位置；请先运行 Remove NVIDIA driver，再重新安装（未更改驱动）";; esac
    [ -d "$bk" ] && [ ! -L "$bk" ] || die "中断安装的备份 $bk 不存在；请先运行 Remove NVIDIA driver，再重新安装（未更改驱动）"
    BK=$bk
    if ! rollback_install; then : > "$g/restore-incomplete"; die "恢复中断的安装失败；备份保留在 $bk"; fi
    echo "   ok  已使用 $bk 回滚中断的安装"; BK=""
  fi
  rm -f "$o" "$g/installing"; rmdir "$g" 2>/dev/null || die "无法清除中断安装遗留的保护标记"
}
die() {
  echo "   STOP: $*" >&2
  if [ "$INSTALLING" = 1 ]; then
    if rollback_install; then echo "   已恢复先前安装；备份保留在 ${BK}。" >&2
    else LOCK_RELEASE_OK=0; : > "$LOCKDIR/restore-incomplete" 2>/dev/null; echo "   恢复未完成；恢复文件保留在 ${BK}。" >&2; fi
  fi
  exit 1
}

# Keep diagnostic output bounded while retaining the copy command's exit status.
backup_copy() {
  local component=$1; shift
  "$@" 2>&1 | tail -c 4096 > "$T/backup-error.log"
  local status=("${PIPESTATUS[@]}")
  if [ "${status[0]}" != 0 ] || [ "${status[1]}" != 0 ]; then
    printf '   BACKUP-DIAGNOSTIC component=%s command-status=%s capture-status=%s stderr-tail-limit=4096\n' "$component" "${status[0]}" "${status[1]}" >&2
    [ ! -f "$T/backup-error.log" ] || cat "$T/backup-error.log" >&2
    echo >&2
    die "备份 $component"
  fi
}
finish_install() { [ -z "$STAGE" ] || rm -rf "$STAGE"; release_install_lock; }
trap finish_install EXIT
trap 'die "安装器被中断（收到 HUP 信号）"' HUP
trap 'die "安装器被中断（收到 INT 信号）"' INT
trap 'die "安装器被中断（收到 TERM 信号）"' TERM

[ "$(id -u)" -eq 0 ] || die "请使用 sudo 运行"
step "1. 本机检查"
[ "$(uname -m)" = x86_64 ] || die "仅支持 Intel/x86_64"
v=$(sw_vers -productVersion); MAJ=${v%%.*}
BUILD=$(sw_vers -buildVersion)
printf '%s\n' "$BUILD" | grep -Eq '^[0-9]+[A-Za-z][A-Za-z0-9]+$' || die "无法识别当前 macOS 构建版本"
case $MAJ in 15|26) ;; *) die "此驱动适用于 macOS 15 和 26（当前为 ${v}）；未作任何更改";; esac
# NVAccel is the one kext built per macOS: Tahoe made the IOAcceleratorFamily2 methods it inherits private (see
# kexts/NVRM/accel/gen_tahoe_fwd.py). The other three kexts are the same binaries on 15 and 26.
VAR="$HERE/Library/NullMoth/kexts/$MAJ"; [ -d "$VAR/NVAccel.kext" ] || die "此软件包不包含 macOS $MAJ 所需的 NVAccel"
kpath() {
  case "$1" in
    NVAccel) echo "$VAR/NVAccel.kext";;
    NVPrimarySelector) echo "$SELECTOR_SOURCE";;
    NMIntelFB) echo "$HERE/Optional/NMIntelFB.kext";;
    NMXAudio) echo "$HERE/Optional/NMXAudio.kext";;
    *) echo "$HERE/Library/Extensions/$1.kext";;
  esac
}
ioreg -r -c IOPCIDevice -d 1 | grep -q '"vendor-id" = <de100000>' || die "未在 PCI 上检测到 NVIDIA GPU"
# The driver runs the cards NVIDIA's GSP firmware supports (Turing and later). The same list the Windows builder uses
# (open-gpu-kernel-modules 620.70.06: 615's list + 2BBD RTX PRO 5500 Blackwell, 2E13 RTX Spark N1X; the datacenter
# VR NVL72 3041 left out like the other datacenter parts). Installing it for an older card (GTX 10 and before)
# would switch the firmware screen off and leave nothing to draw: those PCs run macOS in basic display instead.
SUPPORTED_IDS="
1E02 1E04 1E07 1E30 1E36 1E78 1E81 1E82 1E84 1E87 1E89 1E90 1E91 1E93 1EB0 1EB1 1EB5 1EB6 1EC2 1EC7 1ED0 1ED1 1ED3
1EF5 1F02 1F03 1F06 1F07 1F08 1F0A 1F10 1F11 1F12 1F14 1F15 1F36 1F42 1F47 1F50 1F51 1F54 1F55 1F76 1F82 1F83 1F91
1F95 1F96 1F97 1F98 1F99 1F9C 1F9D 1F9F 1FA0 1FB0 1FB1 1FB2 1FB6 1FB7 1FB8 1FB9 1FBA 1FBB 1FBC 1FDD 1FF0 1FF2 1FF9
2182 2184 2187 2188 2191 2192 21C4 21D1 2203 2204 2206 2207 2208 220A 2216 2230 2231 2232 2233 2414 2420 2438 2460
2482 2484 2486 2487 2488 2489 249C 249D 24A0 24B0 24B1 24B6 24B7 24B8 24B9 24BA 24BB 24C7 24C9 24DC 24DD 24E0 24FA
2503 2504 2507 2508 2520 2521 2523 2531 2544 2560 2563 2571 2582 2584 25A0 25A2 25A5 25A6 25A7 25A9 25AA 25AB 25AC
25AD 25B0 25B2 25B8 25B9 25BA 25BB 25BC 25BD 25E0 25E2 25E5 25EC 25ED 25F9 25FA 25FB 2684 2685 2689 26B1 26B2 26B3
2702 2704 2705 2709 2717 2730 2757 2770 2782 2783 2786 2788 27A0 27B0 27B1 27B2 27BA 27BB 27E0 27FB 2803 2805 2808
2820 2822 2838 2860 2882 28A0 28A1 28A3 28B0 28B8 28B9 28BA 28BB 28E0 28E1 28E3 28F8 2B85 2B87 2B8C 2BB1 2BB3 2BB4
2BB5 2BB9 2BBD 2C02 2C05 2C09 2C18 2C19 2C31 2C33 2C34 2C38 2C39 2C3A 2C58 2C59 2C77 2C79 2D04 2D05 2D18 2D19 2D30
2D39 2D58 2D59 2D79 2D83 2D98 2DB8 2DB9 2DD8 2DF9 2E03 2E06 2E12 2E13 2F04 2F06 2F18 2F38 2F58
"
NV_IDS=$(ioreg -r -c IOPCIDevice -d 1 | awk '/"vendor-id" = <de100000>/{nv=1} /"device-id" = </{ if (match($0, /<[0-9a-f]+>/)) d=substr($0, RSTART+1, 4) }
  /^\+-o|^ *\}/{ if (nv && d != "") print toupper(substr(d,3,2) substr(d,1,2)); nv=0; d="" }' | sort -u)
CARD=""; for i in $NV_IDS; do case " $(echo $SUPPORTED_IDS) " in *" $i "*) CARD=$i;; esac; done
# Every device id from 1E00 up is Turing or newer (TU102 is 1E02; Volta and older sit below it), which RM drives through GSP.
# The list above names the GeForce/RTX ids measured so far; a Quadro T1200 laptop (1FBC) was refused as "older".
[ -n "$CARD" ] || for i in $NV_IDS; do case $i in *[!0-9A-F]*|"") ;; *) [ $((16#$i)) -ge $((16#1E00)) ] && CARD=$i;; esac; done
[ -n "$CARD" ] || die "这张 NVIDIA 显卡（设备 $(echo $NV_IDS)）早于本驱动支持的型号（RTX 20 / Turing 及更新型号）；macOS 会继续使用基本显示模式，未作任何更改"
ok "macOS ${v}，x86_64，已检测到 NVIDIA GPU"

step "2. 软件包完整性检查"
# The extracted package is usually writable by the logged-in user, so a process running as that user could swap a kext
# between this check and the copy into /Library/Extensions. Check and install a root-owned private copy instead.
SELF=$(cd "$(dirname "$0")" && pwd)
STAGE=$(mktemp -d /var/tmp/nullmoth-package.XXXXXX) && [ -n "$STAGE" ] && [ -d "$STAGE" ] || die "无法创建软件包临时副本"
cp -cR "$HERE" "$STAGE/pkgroot" 2>/dev/null || { rm -rf "$STAGE/pkgroot"; ditto "$HERE" "$STAGE/pkgroot"; } && chown -R root:wheel "$STAGE" && chmod -R go-w "$STAGE" || die "无法将软件包复制到 $STAGE"
# shasum follows links: a link to an identical file outside the copy passes the check and can still change later.
while IFS= read -r -d '' link; do
  case "$(readlink "$link")" in /*|..|../*|*/..|*/../*) die "软件包中存在指向软件包目录之外的链接：${link#"$STAGE/pkgroot/"}";; esac
done < <(find "$STAGE/pkgroot" -type l -print0)
[ "$SELF" = "$HERE" ] && SELF="$STAGE/pkgroot"
HERE="$STAGE/pkgroot"
# VAR was set from the original folder before this step, and kpath reads the accelerator kext through it. Point it
# at the private copy too, or that one kernel extension is still taken from the folder the check no longer covers.
VAR="$HERE/Library/NullMoth/kexts/$MAJ"
# --strict: without it shasum skips a malformed line and still reports success
(cd "$HERE" && shasum -a 256 --strict -c SHA256SUMS --quiet) || die "SHA256SUMS 校验不匹配：请重新下载软件包"
# Hash verification alone ignores files absent from the manifest. Check the
# complete private payload, using a checker whose own bytes were just verified.
[ -f "$HERE/sums_complete.sh" ] && [ ! -L "$HERE/sums_complete.sh" ] && \
  grep -qE '^[0-9a-fA-F]{64} [ *](\./)?sums_complete\.sh$' "$HERE/SHA256SUMS" \
  || die "此软件包未包含经过校验的 sums_complete.sh：请重新下载软件包"
bash "$HERE/sums_complete.sh" "$HERE" || die "软件包中存在未列入 SHA256SUMS 的文件：请重新下载软件包"
for target in 15 26; do
  [ -f "$HERE/Library/NullMoth/kexts/$target/NVAccel.kext/Contents/MacOS/NVAccel" ] && \
    [ -f "$HERE/Library/NullMoth/kexts/$target/NVAccel.kext/Contents/Info.plist" ] || die "此软件包缺少完整的 macOS $target 图形加速组件"
done
ok "所有文件均与 SHA256SUMS 匹配，且两个 macOS 版本的图形加速组件均已包含"
# The portable model name can be spoofed on a Hackintosh, and it does not tell
# us which GPU owns the panel. Ask instead of enabling this framebuffer from it.
INTEL_PANEL=0; INTEL_PANEL_TOUCHED=0
if [ -e "$EXT/NMIntelFB.kext" ] || [ -L "$EXT/NMIntelFB.kext" ]; then
  [ ! -L "$EXT/NMIntelFB.kext" ] && [ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$EXT/NMIntelFB.kext/Contents/Info.plist" 2>/dev/null)" = com.nullmoth.NMIntelFB ] \
    || die "无法识别现有的 NMIntelFB.kext；未更改驱动"
  [ "$INTEL_PANEL_CHOICE" != ask ] || INTEL_PANEL_CHOICE=yes
fi
if [ "$INTEL_PANEL_CHOICE" = ask ] && [ -d "$HERE/Optional/NMIntelFB.kext" ]; then
  printf '\n%s\n' '----------------------------------------------------------------------'
  echo "可选的 Intel 笔记本内屏支持（实验性）："
  printf '%s\n\n' '----------------------------------------------------------------------'
  echo "仅用于内置屏连接到 macOS 不支持的 Intel GPU 的笔记本。"
  echo "台式机、macOS 支持的 Intel 显卡，或由 NVIDIA 直接驱动内屏的设备请跳过。"
  echo "此选项沿用固件显示模式；NVIDIA 到 Intel 的内屏加速仍默认关闭。"
  if [ -t 0 ]; then
    printf '是否为此笔记本内屏安装 NMIntelFB？[y/N] '
    answer=""; IFS= read -r answer || answer=""
    case "$answer" in y|Y|yes|YES|Yes) INTEL_PANEL_CHOICE=yes;; *) INTEL_PANEL_CHOICE=no;; esac
  else
    echo "No interactive input: 未提供交互输入，已跳过 Intel 内屏支持（使用 --intel-panel=yes 可启用）。"
    INTEL_PANEL_CHOICE=no
  fi
fi
if [ "$INTEL_PANEL_CHOICE" = yes ]; then
  [ -d "$HERE/Optional/NMIntelFB.kext" ] && [ ! -L "$HERE/Optional/NMIntelFB.kext" ] \
    || die "此软件包缺少可选的 Intel 内屏 kext"
  [ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$HERE/Optional/NMIntelFB.kext/Contents/Info.plist" 2>/dev/null)" = com.nullmoth.NMIntelFB ] \
    || die "可选 Intel 内屏组件的 Bundle 标识无效"
  codesign --verify --strict "$HERE/Optional/NMIntelFB.kext" || die "Intel 内屏 kext 签名无效"
  KEXTS="$KEXTS NMIntelFB"; INTEL_PANEL=1; INTEL_PANEL_TOUCHED=1
  ok "已选择实验性 Intel 内屏支持；更新时会保留此前的启用设置"
elif [ -d "$EXT/NMIntelFB.kext" ]; then
  REMOVED_KEXTS=NMIntelFB; INTEL_PANEL_TOUCHED=1
  ok "将移除 Intel 内屏支持；旧版本已备份"
fi
# NMXAudio is a separate, optional HD Audio driver for Tahoe. Never infer an
# opt-in from a detected controller or take ownership of another installation.
AUDIO=0; AUDIO_TOUCHED=0; AUDIO_OWNED=0
audio_managed && AUDIO_OWNED=1
if [ "$AUDIO_CHOICE" = ask ] && [ "$AUDIO_OWNED" = 1 ]; then AUDIO_CHOICE=yes; fi
if [ "$AUDIO_CHOICE" = ask ]; then
  AUDIO_CHOICE=no
  if [ "$MAJ" -ge 26 ] && [ -d "$HERE/Optional/NMXAudio.kext" ] && [ ! -e "$EXT/NMXAudio.kext" ] &&
     ioreg -r -c IOPCIDevice -l -w0 2>/dev/null | grep -q '"class-code" = <00030400>'; then
    printf '\n%s\n' '----------------------------------------------------------------------'
    echo "可选的 NMXAudio 高清音频驱动（仅 Tahoe）。如果音频已正常工作，请跳过。"
    printf '%s\n\n' '----------------------------------------------------------------------'
    if [ -t 0 ]; then
      printf '是否安装可选音频驱动？[y/N] '
      answer=""; IFS= read -r answer || answer=""
      case "$answer" in y|Y|yes|YES|Yes) AUDIO_CHOICE=yes;; esac
    else
      echo "No interactive input: 未提供交互输入，已跳过音频支持（使用 --audio=yes 可启用）。"
    fi
  fi
fi
if [ "$AUDIO_CHOICE" = yes ]; then
  [ "$MAJ" -ge 26 ] || die "可选音频驱动需要 macOS Tahoe；在 Sequoia 上请使用 --audio=no"
  [ "$AUDIO_OWNED" = 1 ] || { [ ! -e "$EXT/NMXAudio.kext" ] && [ ! -L "$EXT/NMXAudio.kext" ]; } \
    || die "NMXAudio 是独立安装的或已被修改，已保持原样（请使用 --audio=no）"
  [ -d "$HERE/Optional/NMXAudio.kext" ] && [ ! -L "$HERE/Optional/NMXAudio.kext" ] \
    || die "此软件包缺少可选音频驱动"
  [ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$HERE/Optional/NMXAudio.kext/Contents/Info.plist" 2>/dev/null)" = com.nullmoth.NMXAudio ] \
    || die "可选音频组件的 Bundle 标识无效"
  ioreg -r -c IOPCIDevice -l -w0 2>/dev/null | grep -q '"class-code" = <00030400>' \
    || die "未检测到 PCI 高清音频控制器；请使用 --audio=no"
  KEXTS="$KEXTS NMXAudio"; AUDIO=1; AUDIO_TOUCHED=1
  ok "已选择可选音频驱动；更新时会保留此前由驱动管理的启用设置"
elif [ "$AUDIO_OWNED" = 1 ]; then
  REMOVED_KEXTS="$REMOVED_KEXTS NMXAudio"; AUDIO_TOUCHED=1
  ok "将移除由本驱动管理的音频组件；独立安装的音频驱动会保留"
fi
GLD_BUNDLE=""
if [ -d "$HERE/Library/GPUBundles/NVKZinkGLDriver.bundle" ]; then
  GLD_BUNDLE=NVKZinkGLDriver.bundle
  selected_gl=$(/usr/libexec/PlistBuddy -c 'Print :IOKitPersonalities:NVAccel:IOGLBundleName' "$VAR/NVAccel.kext/Contents/Info.plist" 2>/dev/null || true)
  [ "$selected_gl" = NVKZinkGLDriver ] || die "此软件包未为 macOS $MAJ 启用原生 Zink（未更改驱动）"
  [ -x "$HERE/Library/GPUBundles/nvmtl/zink/gld_bundle_probe" ] || die "缺少原生 GLD 查找检查工具"
fi
RUNTIME_PREFLIGHT="$SELF/nullmoth-runtime-check.sh"
[ -x "$RUNTIME_PREFLIGHT" ] || die "缺少运行环境兼容性检查工具"
/bin/bash "$RUNTIME_PREFLIGHT" "$HERE" "$v" || die "用户态运行组件无法在此 macOS 版本上加载"

require_install_directory() {
  local metadata permissions entry listing grants
  [ ! -L "$1" ] && [ -d "$1" ] || die "安装器所在目录必须是实际目录（未更改驱动）"
  metadata=$(stat -f '%u %Lp' "$1" 2>/dev/null) || die "无法验证安装器所在目录的所有权"
  case "$metadata" in
    "0 "[0-7][0-7][0-7]|"0 "[0-7][0-7][0-7][0-7]) ;;
    *) die "安装器所在目录不是已验证的 root 所有目录（未更改驱动）";;
  esac
  permissions=${metadata#* }
  [ $((8#$permissions & 022)) -eq 0 ] || die "其他账户或用户组可以写入安装器所在目录（未更改驱动）"
  listing=$(ls -lde "$1" 2>/dev/null)
  entry=$(printf '%s\n' "$listing" | head -1 | awk '{print $1}')
  [ -n "$entry" ] || die "无法检查安装器所在目录的访问控制设置"
  # A deny entry only takes access away (some Macs carry "group:everyone deny delete" on /Library); an allow entry can
  # hand another account write access the mode bits above never show, so any allow entry still stops the install and
  # is named in the log.
  case "$entry" in *+*)
    grants=$(printf '%s\n' "$listing" | tail -n +2 | grep -E '^[[:space:]]*[0-9]+:.*[[:space:]]allow[[:space:]]' || true)
    [ -z "$grants" ] || die "安装器所在目录 $1 存在授予访问权限的访问控制项（$(printf '%s' "$grants" | tr -s ' \n' ' ')）（未更改驱动）"
    ;;
  esac
}

# The installer owns this guard from preflight through commit/rollback. Never
# remove a pre-existing guard: it may belong to an active or interrupted install.
if [ "${CHECK:-0}" != 1 ]; then
  require_install_directory /Library
  if [ ! -e /Library/NullMoth ] && [ ! -L /Library/NullMoth ]; then
    mkdir -m 755 /Library/NullMoth || die "无法创建安装器状态目录"
  fi
  require_install_directory /Library/NullMoth
  if ! mkdir -m 700 "$LOCKDIR" 2>/dev/null; then
    recover_stale_guard
    mkdir -m 700 "$LOCKDIR" 2>/dev/null || die "安装保护标记已存在；当前已有另一个安装任务正在运行（未更改驱动）"
  fi
  LOCK_HELD=1
  printf 'pid=%s\nboot=%s\n' "$$" "$(boot_sec)" > "$LOCKDIR/owner" || die "无法记录安装保护标记的所有者"
  LOCK_ID=$(stat -f '%d:%i' "$LOCKDIR" 2>/dev/null) || die "无法记录安装保护标记的身份"
  printf '%s\n' "$LOCK_ID" | grep -Eq '^[0-9]+:[0-9]+$' || die "安装保护标记的身份不可用"
fi

primary_bundle_id() {
 if [ "$1" = NVPrimarySelector ]; then echo org.hackintosh.experimental.NVPrimarySelector
 else echo "com.nullmoth.$1"; fi
}
primary_marker() {
 /usr/libexec/PlistBuddy -c "Print :IOKitPersonalities:$2:IOPropertyMatch:nullmoth-auto-primary" "$1/Contents/Info.plist" 2>/dev/null
}
primary_apply() {
 local bundle=$1 name=$2 plist="$1/Contents/Info.plist" existing
 existing=$(/usr/libexec/PlistBuddy -c "Print :IOKitPersonalities:$name:IOPropertyMatch" "$plist" 2>/dev/null) || existing=''
 if [ -n "$existing" ]; then
  # Do not replace a different PCI/ACPI selection or silently widen its scope.
  [ "$(primary_marker "$bundle" "$name")" = true ] || return 1
  existing=$(/usr/libexec/PlistBuddy -c "Print :IOKitPersonalities:$name:IOPropertyMatch" "$plist") || return 1
  [ "$(printf '%s\n' "$existing" | awk '/ = /{n++}END{print n+0}')" = 1 ] || return 1
 else
  /usr/libexec/PlistBuddy -c "Add :IOKitPersonalities:$name:IOPropertyMatch dict" "$plist" || return 1
  /usr/libexec/PlistBuddy -c "Add :IOKitPersonalities:$name:IOPropertyMatch:nullmoth-auto-primary bool true" "$plist" || return 1
 fi
 codesign --force --sign - "$bundle" && codesign --verify --strict "$bundle"
}
primary_requested() {
 local rm accel
 rm=$(primary_marker "$EXT/NVRM.kext" NVRM) || rm=''
 accel=$(primary_marker "$EXT/NVAccel.kext" NVAccel) || accel=''
 if [ "$rm" = true ] || [ "$accel" = true ]; then
  [ "$rm" = true ] && [ "$accel" = true ] || return 2
  return 0
 fi
 [ "${NM_PRIMARY_GPU:-0}" = 1 ]
}

# An already configured selector is an opt-in that must survive a driver update.
# NM_PRIMARY_GPU=1 is for a first manual installation with a packaged selector.
PRIMARY=0
primary_requested; primary_status=$?
case "$primary_status" in
 0) PRIMARY=1;;
 2) die "已安装的 kext 中主 GPU 匹配设置不完整；未更改任何文件";;
esac
SELECTOR_SOURCE=""
if [ "$PRIMARY" = 1 ]; then
  SELECTOR_SOURCE="$HERE/Library/Extensions/NVPrimarySelector.kext"
  [ -d "$SELECTOR_SOURCE" ] || SELECTOR_SOURCE="$EXT/NVPrimarySelector.kext"
  [ -d "$SELECTOR_SOURCE" ] && [ ! -L "$SELECTOR_SOURCE" ] || die "主 GPU 匹配需要 NVPrimarySelector.kext；未更改任何文件"
  codesign --verify --strict "$SELECTOR_SOURCE" || die "主 GPU 选择器签名无效；未更改任何文件"
  KEXTS="$KEXTS NVPrimarySelector"
fi
# kmutil builds the auxiliary collection from every kext in /Library/Extensions, so one that names a dependency this
# macOS lacks stops the build for all of them. 2.5.3 report: a Realtek USB Wi-Fi kext (RtWlanU1827) needed
# com.apple.iokit.IOUSBFamily, which macOS 15 no longer has, and the stop said only "test kernel collection build
# failed". This names that kext, where it is, and what to do; it is not one of ours, so it is never moved or changed here.
kmutil_blocker() {  # <kmutil log> <extensions folder>: prints a sentence for the user, or nothing
  local log=$1 ext=$2 line id dep path x
  line=$(grep -m1 -oE "Unable to resolve dependencies: '[^']+' names a dependency on '[^']+'" "$log" 2>/dev/null) || return 0
  id=$(printf '%s' "$line" | sed -E "s/^Unable to resolve dependencies: '([^']+)'.*/\1/")
  dep=$(printf '%s' "$line" | sed -E "s/.*names a dependency on '([^']+)'$/\1/")
  case "$id" in com.nullmoth.*|org.hackintosh.experimental.NVPrimarySelector) return 0;; esac
  path=""
  for x in "$ext"/*.kext; do
    [ -f "$x/Contents/Info.plist" ] || continue
    [ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$x/Contents/Info.plist" 2>/dev/null)" = "$id" ] && { path=$x; break; }
  done
  printf '%s 需要依赖 %s，但当前 macOS 不包含该依赖，因此它留在此处时 macOS 无法构建内核集合。它不属于本驱动。请将 %s 移出 %s（或改为通过 OpenCore 加载），然后重新运行 1401。' \
    "$id" "$dep" "${path:-the kext with that identifier}" "$ext"
}
# Shared with uninstall.sh: resolve old staging references by bundle identity,
# never by filename. Existing permanent paths may contain a pending update;
# recovery of a missing/temporary path requires the recorded version too.
permanent_kext_path() {
  local wanted_id=$1 wanted_version=$2 recorded=$3 candidate resolved found="" id version
  if [ -d "$recorded" ]; then
    resolved=$(cd "$recorded" && pwd -P) || return 1
    case "$resolved" in /private/var/tmp/*|/var/tmp/*|/private/tmp/*|/tmp/*) ;;
      *)
        id=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$resolved/Contents/Info.plist" 2>/dev/null) || id=''
        if [ "$id" = "$wanted_id" ]; then printf '%s\n' "$resolved"; return 0; fi
        ;;
    esac
  fi
  for candidate in "$EXT"/*.kext /Library/Apple/System/Library/Extensions/*.kext; do
    [ -d "$candidate" ] && [ ! -L "$candidate" ] || continue
    id=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$candidate/Contents/Info.plist" 2>/dev/null) || continue
    [ "$id" = "$wanted_id" ] || continue
    version=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$candidate/Contents/Info.plist" 2>/dev/null) || continue
    [ -n "$wanted_version" ] && [ "$version" = "$wanted_version" ] || continue
    resolved=$(cd "$candidate" && pwd -P) || return 1
    if [ -n "$found" ] && [ "$found" != "$resolved" ]; then
      echo "发现多个已安装的 $wanted_id $wanted_version 版本，无法确定要使用哪一个。" >&2; return 1
    fi
    found=$resolved
  done
  [ -n "$found" ] || { echo "未找到 $wanted_id $wanted_version 的已安装版本（记录路径：${recorded}）。" >&2; return 1; }
  printf '%s\n' "$found"
}

step "3. 测试内核集合"
T=$(mktemp -d /var/tmp/nullmoth.XXXX) && [ -n "$T" ] && mkdir -p "$T/repo" || die "无法创建预检临时目录"
# Preserve installed extensions outside the default repositories (for example
# MacPorts macFUSE). Rebuilding only /Library/Extensions silently drops them.
# Use their permanent paths, not disposable preflight/staging copies.
EXTERNAL_KEXT_ARGS=(); EXTERNAL_KEXT_IDS=""
if [ -s "$KC" ]; then
  kmutil inspect -a x86_64 -A "$KC" --show-extension-info > "$T/existing-kexts" 2>/dev/null \
    || die "无法检查现有内核集合，不能保留外部扩展"
  while IFS=$'\t' read -r external_id external_version external_path external_rest; do
    case "$external_path" in *.kext) ;; *) continue;; esac
    case "$external_path" in "$EXT"/*|/Library/Apple/System/Library/Extensions/*|/System/*) continue;; esac
    printf '%s' "$external_id" | grep -Eq '^[A-Za-z0-9_.-]+$' || die "外部扩展的标识无效"
    case "$external_id" in com.nullmoth.NVRM|com.nullmoth.NVAccel|com.nullmoth.NVRMFB|com.nullmoth.NVRMAGDC|com.nullmoth.NMIntelFB) continue;; esac
    permanent_path=$(permanent_kext_path "$external_id" "$external_version" "$external_path") \
      || die "无法从永久安装路径保留现有扩展 $external_id"
    parent=$permanent_path
    while :; do
      require_install_directory "$parent"
      [ "$parent" != / ] || break
      parent=$(dirname "$parent")
    done
    EXTERNAL_KEXT_ARGS+=(-p "$permanent_path")
    EXTERNAL_KEXT_IDS="$EXTERNAL_KEXT_IDS $external_id"
  done < "$T/existing-kexts"
fi
for x in "$EXT"/*.kext; do [ -d "$x" ] || continue; n=$(basename "$x" .kext); case " $KEXTS $REMOVED_KEXTS " in *" $n "*) ;; *) cp -R "$x" "$T/repo/" || die "无法暂存第三方 kext $n";; esac; done
for k in $KEXTS; do cp -R "$(kpath $k)" "$T/repo/" || die "无法复制 $k"; done
if [ "$PRIMARY" = 1 ]; then
  primary_apply "$T/repo/NVRM.kext" NVRM && primary_apply "$T/repo/NVAccel.kext" NVAccel \
      || die "无法准备主 GPU 元数据；未更改已安装文件"
fi
# macOS 26 kmutil silently skips kexts not owned by root ("No binaries or codeless kexts were provided").
chown -R root:wheel "$T/repo" && chmod -R go-w "$T/repo" || die "无法设置预检 kext 的权限"
kmutil create -n aux --volume-root / ${KARG[@]+"${KARG[@]}"} -B $KB -S $KS --repository "$T/repo" ${EXTERNAL_KEXT_ARGS[@]+"${EXTERNAL_KEXT_ARGS[@]}"} -A "$T/aux.kc" -z >"$T/kmutil.log" 2>&1 || { tail -20 "$T/kmutil.log"; why=$(kmutil_blocker "$T/kmutil.log" "$EXT"); die "内核集合测试构建失败${why:+：$why}"; }
[ -s "$T/aux.kc" ] || die "内核集合测试构建未生成结果文件"
INS=$(kmutil inspect -a x86_64 -A "$T/aux.kc" 2>/dev/null) || die "无法检查测试内核集合"
for k in $KEXTS; do printf '%s\n' "$INS" | grep -oE '(com\.nullmoth\.[A-Za-z0-9]+|org\.hackintosh\.experimental\.NVPrimarySelector)' | grep -Fxq "$(primary_bundle_id "$k")" || { tail -20 "$T/kmutil.log"; die "kmutil 未接受 com.nullmoth.${k}（见上方日志）"; }; done
for k in $REMOVED_KEXTS; do
  if printf '%s\n' "$INS" | grep -oE 'com\.nullmoth\.[A-Za-z0-9]+' | grep -Fxq "$(primary_bundle_id "$k")"; then
    die "测试集合仍包含已取消选择的 ${k}；未更改已安装文件"
  fi
done
for external_id in $EXTERNAL_KEXT_IDS; do
  printf '%s\n' "$INS" | awk '{print $1}' | grep -Fxq "$external_id" || die "测试集合未包含 $external_id"
done
ok "测试集合已包含所有选定的 kext"
[ "${CHECK:-0}" = 1 ] && { rm -rf "$T"; echo; echo "CHECK PASS (nothing changed)"; exit 0; }

BK=$(mktemp -d "/Library/NullMoth/backup-$(date +%Y%m%d-%H%M%S).XXXXXX") && [ -n "$BK" ] && [ -d "$BK" ] || die "无法创建唯一的备份目录"
[ "${CHECK:-0}" = 1 ] || printf 'bk=%s\n' "$BK" >> "$LOCKDIR/owner" || die "无法在安装保护标记中记录备份位置"
step "4. 备份当前文件 -> $BK"
printf '%s\n' "$BUILD" > "$BK/macos-build" || die "无法记录备份对应的 macOS 构建版本"
[ "$PRIMARY" != 1 ] || touch "$BK/primary-selector-used" || die "无法记录主 GPU 选择器备份"
[ "$INTEL_PANEL_TOUCHED" != 1 ] || touch "$BK/intel-panel-touched" || die "无法记录 Intel 内屏回滚状态"
if [ "$AUDIO_TOUCHED" = 1 ]; then
  touch "$BK/audio-touched" || die "无法记录音频组件回滚状态"
  [ ! -f "$AUDIO_MARKER" ] || backup_copy "audio opt-in" cp -p "$AUDIO_MARKER" "$BK/nvidia-audio.sha256"
fi
CHANGED_KEXTS="$KEXTS${REMOVED_KEXTS:+ $REMOVED_KEXTS}"
# ditto, not cp -Rp: cp re-applies file flags through symlinks and stopped on nvmtl/libvulkan.dylib with "chflags: Too
# many levels of symbolic links" (Mac 1.10 logs 10-10, 2 uploads); ditto keeps links, flags and xattrs, as the restore does.
for k in $KEXTS $REMOVED_KEXTS; do [ ! -e "$EXT/$k.kext" ] || backup_copy "$k" ditto "$EXT/$k.kext" "$BK/$k.kext"; done
for b in NVMTLDriver.bundle NVIDIAShared.bundle nvmtl NVKZinkGLDriver.bundle nvmtl-allow.txt; do [ ! -e "$GB/$b" ] || backup_copy "$b" ditto "$GB/$b" "$BK/$b"; done
[ ! -f "$KC" ] || backup_copy "kernel collection" cp -p "$KC" "$BK/AuxiliaryKernelExtensions.kc"
[ ! -d /Library/NullMoth/kexts ] || backup_copy "cached accelerators" ditto /Library/NullMoth/kexts "$BK/kexts"
[ ! -f /Library/NullMoth/os-major ] || backup_copy "OS record" cp -p /Library/NullMoth/os-major "$BK/os-major"
[ ! -f /Library/NullMoth/driver-version ] || backup_copy "version record" cp -p /Library/NullMoth/driver-version "$BK/driver-version"
# Back up the firmware from wherever it actually is. This version reads it from $FW, but a release
# before the move kept it in $FWOLD, so on an upgrade $FW does not exist yet and backing up only $FW
# saved nothing. Both restore paths (rollback_install and the boot-guard fallback) are gated on
# $BK/nvfw, so an empty backup made them no-ops: the fallback put the previous driver back with NO
# firmware at all and the Mac came up with no display and no way out. Measured on the test Mac.
if [ -d "$FW" ]; then backup_copy firmware ditto "$FW" "$BK/nvfw"
# The old location is backed up only if root owns it and no one else can write it: a folder another account planted in
# /Users/Shared would otherwise become the backup, and a rollback would copy that account's files into $FW, which the
# kernel reads.
elif [ -d "$FWOLD" ] && [ ! -L "$FWOLD" ] && [ "$(stat -f '%u %Lp' "$FWOLD" | awk '{g=substr($2,length($2)-1,1); o=substr($2,length($2),1); print ($1==0 && g !~ /[2367]/ && o !~ /[2367]/) ? "ours" : "not"}')" = ours ]; then backup_copy firmware ditto "$FWOLD" "$BK/nvfw"
fi
ok "备份已完成"

step "5. 安装"
INSTALLING=1; [ "${CHECK:-0}" = 1 ] || : > "$LOCKDIR/installing"
for k in $REMOVED_KEXTS; do rm -rf "$EXT/$k.kext" || die "无法移除 ${k}（从 ${BK} 恢复）"; done
for k in $KEXTS; do
  source=$(kpath "$k")
  if [ "$PRIMARY" = 1 ]; then source="$T/repo/$k.kext"; fi
  rm -rf "$EXT/$k.kext" && ditto "$source" "$EXT/$k.kext" || die "无法复制 ${k}（从 ${BK} 恢复）"
done
# both NVAccel builds stay on disk, so the first start after a macOS upgrade can switch to the matching one
mkdir -p /Library/NullMoth && rm -rf /Library/NullMoth/kexts && ditto "$HERE/Library/NullMoth/kexts" /Library/NullMoth/kexts || die "无法复制各 macOS 版本的图形加速组件（从 $BK 恢复）"
for target in 15 26; do
  dir=/Library/NullMoth/kexts/$target
  if [ "$PRIMARY" = 1 ]; then
    primary_apply "$dir/NVAccel.kext" NVAccel || die "无法写入 macOS $target 的主 GPU 缓存元数据（从 $BK 恢复）"
  fi
  [ -f "$dir/NVAccel.kext/Contents/MacOS/NVAccel" ] && [ -f "$dir/NVAccel.kext/Contents/Info.plist" ] || die "缺少 macOS $target 图形加速组件（从 $BK 恢复）"
  (cd "$dir" && find NVAccel.kext -type f -exec shasum -a 256 '{}' \;) > "$dir/SHA256SUMS" || die "无法生成 macOS $target 的缓存校验清单"
done
chown -R root:wheel /Library/NullMoth/kexts && chmod -R go-w /Library/NullMoth/kexts || die "无法设置缓存图形加速组件的权限"
echo "$MAJ" > /Library/NullMoth/os-major || die "无法记录当前 macOS 版本"
mkdir -p "$GB" || die "无法创建图形组件目录"
# The card runs these images and the kernel reads them, so this directory must be ours. It used to live in
# /Users/Shared, which is world-writable (1777): a directory left there by anyone else was adopted as-is,
# because the only permission step was a chmod that adds read bits and never takes write away. Refuse
# anything we did not make, the way the shader caches already refuse a cache someone else could have written.
if [ -e "$FW" ] && [ -n "$(find "$FW" -maxdepth 0 \( ! -user root -o -perm -o+w -o -perm -g+w \) -print -quit 2>/dev/null)" ]; then
  rm -rf "$FW" || die "无法清除非 root 所有的固件目录"
  echo "   note: 固件目录对其他用户开放了写入权限，现已重新创建"
fi
mkdir -p "$FW" || die "无法创建固件目录"
# $GB/nvmtl is replaced wholesale below, so move the shared shader caches aside first. NVMTLLibraryLoad.m
# reads them from NVMTL_AIRCACHE_DIR (/Library/GPUBundles/nvmtl/aircache) and uses that path whenever it is
# writable, which is what lets every process - WindowServer included - share one persistent cache instead of
# compiling into a private one that a reboot or a temp sweep throws away.
CACHEKEPT=""
for c in aircache spvcache; do
  [ ! -d "$GB/nvmtl/$c" ] || { mv "$GB/nvmtl/$c" "$T/keep-$c" && CACHEKEPT="$CACHEKEPT $c" || die "无法暂存 $GB/nvmtl/$c"; }
done
# A user's own settings survive updates: the lines between these two markers in nvrm610.conf are put back after the
# copy below. An RTX 3050 (GA106, GH #130) ran only with NVMTL_NO_SURFACE_PAGEOFF=1 and uncached command pools, and every
# update wrote the file fresh and dropped them. Only driver knobs (NVMTL_/NVK_/NVRM_ with plain values) are kept.
MYCONF_START='# --- my settings, kept on update ---'; MYCONF_END='# --- end my settings ---'
[ ! -f "$GB/nvmtl/nvrm610.conf" ] || sed -n "/^$MYCONF_START\$/,/^$MYCONF_END\$/p" "$GB/nvmtl/nvrm610.conf" \
  | grep -E "^($MYCONF_START|$MYCONF_END|(NVMTL|NVK|NVRM)_[A-Z0-9_]+=[A-Za-z0-9._-]{0,64})\$" > "$T/my-conf" || : > "$T/my-conf"
for b in NVMTLDriver.bundle NVIDIAShared.bundle nvmtl ${GLD_BUNDLE:+"$GLD_BUNDLE"}; do rm -rf "$GB/$b" && ditto "$HERE/Library/GPUBundles/$b" "$GB/$b" || die "无法复制 ${b}（从 ${BK} 恢复）"; done
[ -n "$GLD_BUNDLE" ] || rm -rf "$GB/NVKZinkGLDriver.bundle" || die "无法移除旧的可选 OpenGL 组件（从 $BK 恢复）"
if [ -s "$T/my-conf" ] && [ -f "$GB/nvmtl/nvrm610.conf" ]; then
  cat "$T/my-conf" >> "$GB/nvmtl/nvrm610.conf" || die "无法保留 nvrm610.conf 中的自定义设置（从 $BK 恢复）"
  ok "已保留 nvrm610.conf 中的自定义设置（$(grep -cvE '^#' "$T/my-conf") 行）"
fi
# nvmtl was just replaced wholesale: when the separate AMD/Intel driver is installed, its Vulkan drivers (which its
# own installer put in /Library/NullMoth/gpu-userland) go back in beside NVK
if [ -d "$EXT/NMXGPU.kext" ] && [ -d /Library/NullMoth/gpu-userland ]; then
  ditto /Library/NullMoth/gpu-userland "$GB/nvmtl" || die "无法复制 AMD/Intel Vulkan 驱动（从 $BK 恢复）"
fi
cp "$HERE/Library/GPUBundles/nvmtl-allow.txt" "$GB/" || die "无法复制图形组件许可列表"
ditto "$HERE/Users/Shared/nvfw" "$FW" || die "无法复制固件"
chown -R root:wheel "$FW" && chmod -R go-w,a+rX "$FW" || die "无法设置固件权限"
# nothing reads the old location any more; leaving it behind leaves a writable folder with our name on it
rm -rf "$FWOLD" 2>/dev/null || true
for k in $KEXTS; do chown -R root:wheel "$EXT/$k.kext" && chmod -R 755 "$EXT/$k.kext" || die "无法设置 $k 的权限"; done
# An update replaces kexts the user already allowed. macOS keeps that approval per bundle (kext_policy; these kexts have
# no team) and the code hash of the copy it last loaded from each path (kext_load_history_v3). For a kext the user
# already allowed at this path, record the new hash, so the record matches what the next start loads. A kext never
# allowed is left to macOS. The database is SIP-protected; the driver's SIP setting (0x0A43) allows root to write it.
# Measured on an update (Core Ultra 5 225F, macOS 26): while this script replaces the loaded kexts macOS shows a
# "kext upgrade" notice, but the first restart loads the new kexts from the collection built below (kernelmanagerd:
# every one "approved", 0 filtered), so an update needs one restart. ALLOWED counts the kexts macOS already allows.
KPDB=/var/db/SystemPolicyConfiguration/KextPolicy; ALLOWED=0
if [ -f "$KPDB" ] && [ -x /usr/bin/sqlite3 ]; then
  for k in $KEXTS; do
    id=$(primary_bundle_id "$k"); p="$EXT/$k.kext"
    printf '%s' "$id" | grep -Eq '^[A-Za-z0-9.]+$' || continue
    [ "$(/usr/bin/sqlite3 "$KPDB" "select allowed from kext_policy where ifnull(team_id,'')='' and bundle_id='$id'" 2>/dev/null)" = 1 ] || continue
    ALLOWED=$((ALLOWED + 1))
    # only NVRM is signed in the package; macOS records the unsigned ones' hashes itself
    ch=$(codesign -dvvv "$p" 2>&1 | awk -F= '/^CDHash=/{print $2; exit}')
    printf '%s' "$ch" | grep -Eq '^[0-9a-f]{40}$' || continue
    /usr/bin/sqlite3 "$KPDB" "update kext_load_history_v3 set cdhash='$ch' where path='$p' and bundle_id='$id' and ifnull(team_id,'')=''" 2>/dev/null \
      || echo "   note: $k 的批准记录当前为只读；macOS 可能会再次要求批准"
  done
fi
chown -R root:wheel ${GLD_BUNDLE:+"$GB/$GLD_BUNDLE"} "$GB/NVMTLDriver.bundle" "$GB/NVIDIAShared.bundle" "$GB/nvmtl" "$GB/nvmtl-allow.txt" || die "无法设置图形组件权限"
# WindowServer runs as _windowserver and must read the bundles and the allow list. WAS: cp under the app's privileged
# helper (umask 077) left nvmtl-allow.txt at 0600, WindowServer got no Metal device, and CoreDisplay aborted
# ("Failed to create MetalDevice") on every start: the crash loop after install on many machines.
chmod -R go-w,a+rX ${GLD_BUNDLE:+"$GB/$GLD_BUNDLE"} "$GB/NVMTLDriver.bundle" "$GB/NVIDIAShared.bundle" "$GB/nvmtl" && chmod 644 "$GB/nvmtl-allow.txt" || die "无法设置图形组件权限"
sudo -u _windowserver /bin/test -r "$GB/nvmtl-allow.txt" || die "WindowServer 无法读取 $GB/nvmtl-allow.txt（从 $BK 恢复）"
# Create the shared shader caches the plugin looks for. Without them NVMTL_AIRCACHE_DIR never resolves, so
# WindowServer, which compiles the compositor's pipelines on demand, re-translates them from scratch at every start
# and its content renders black until the translation finishes (community patch #19; ownership below).
# NOTE: this must stay AFTER the "chmod -R go-w ... $GB/nvmtl" above. That recursive chmod would otherwise
# strip the write bits back off aircache/spvcache and the shared cache would silently stop working.
mkdir -p "$GB/nvmtl/aircache" "$GB/nvmtl/spvcache" || die "无法创建共享着色器缓存目录"
# Owned by WindowServer (_windowserver), writable by nobody else: WindowServer loads what is in these caches as
# compiled shaders, so a cache any user can write (1777, as first proposed) would let a local user plant code into
# WindowServer. Other processes see the folders read-only and keep their own private caches, which the plugin already
# does when NVMTL_AIRCACHE_DIR is not writable. A cache kept from an earlier install is reused only if nobody but
# _windowserver could have written into it; otherwise it starts empty.
for c in $CACHEKEPT; do
  [ -d "$T/keep-$c" ] || continue
  if [ -z "$(find "$T/keep-$c" \( ! -user _windowserver -o -perm -o+w -o -perm -g+w \) -print -quit 2>/dev/null)" ]; then
    rm -rf "$GB/nvmtl/$c" && mv "$T/keep-$c" "$GB/nvmtl/$c" || die "无法恢复 $GB/nvmtl/$c"
  else rm -rf "$T/keep-$c"; echo "   note: $c 可被其他用户写入，现以空缓存启动"; fi
done
chown -R _windowserver:wheel "$GB/nvmtl/aircache" "$GB/nvmtl/spvcache" || die "无法设置着色器缓存所有者"
chmod 0755 "$GB/nvmtl/aircache" "$GB/nvmtl/spvcache" || die "无法设置共享着色器缓存权限"
if [ -n "$GLD_BUNDLE" ]; then
  "$GB/nvmtl/zink/gld_bundle_probe" NVKZinkGLDriver || die "macOS 不信任已安装的 OpenGL 组件（从 $BK 恢复）"
fi
NEWKC="$KC.nullmoth-install-new"; rm -f "$NEWKC"
kmutil create -n aux --volume-root / ${KARG[@]+"${KARG[@]}"} -B $KB -S $KS --repository "$EXT" ${EXTERNAL_KEXT_ARGS[@]+"${EXTERNAL_KEXT_ARGS[@]}"} -A "$NEWKC" -z >"$T/kmutil2.log" 2>&1 || die "正式内核集合构建失败（从 $BK 恢复）"
[ -s "$NEWKC" ] || die "正式内核集合构建未生成结果文件（从 $BK 恢复）"
INS=$(kmutil inspect -a x86_64 -A "$NEWKC" 2>/dev/null) || die "无法检查新内核集合（从 $BK 恢复）"
for k in $KEXTS; do printf '%s\n' "$INS" | grep -oE '(com\.nullmoth\.[A-Za-z0-9]+|org\.hackintosh\.experimental\.NVPrimarySelector)' | grep -Fxq "$(primary_bundle_id "$k")" || die "新内核集合缺少 com.nullmoth.${k}（从 ${BK} 恢复）"; done
for k in $REMOVED_KEXTS; do
  if printf '%s\n' "$INS" | grep -oE 'com\.nullmoth\.[A-Za-z0-9]+' | grep -Fxq "$(primary_bundle_id "$k")"; then
    die "新内核集合仍包含已取消选择的 ${k}（从 ${BK} 恢复）"
  fi
done
for external_id in $EXTERNAL_KEXT_IDS; do
  printf '%s\n' "$INS" | awk '{print $1}' | grep -Fxq "$external_id" || die "新内核集合未包含 ${external_id}（从 ${BK} 恢复）"
done
mv -f "$NEWKC" "$KC" || die "无法启用新内核集合（从 $BK 恢复）"
INSTALLING=0
rm -rf "$T"
ok "安装完成"
# Record the version for the app's update check. Only the app's setup wrote it, so installs from the release tar left it
# empty and the app kept offering the version already installed (10-10 chat, 1.10.0).
# the licences and notices of everything this package carries, beside the driver they cover
if [ -d "$HERE/licenses" ]; then rm -rf /Library/NullMoth/licenses && ditto "$HERE/licenses" /Library/NullMoth/licenses \
  && chmod -R go-w /Library/NullMoth/licenses || echo "   note: 无法将许可证文本复制到 /Library/NullMoth/licenses"; fi
# /Library/NullMoth must stay traversable by every user: ARM apps' nm-arm lives under it and runs as the user. A root
# shell with umask 077 (the Mac app's privileged steps) left it 700 in 2.7, which hid nm-arm from its own user.
mkdir -p /Library/NullMoth && chmod 755 /Library/NullMoth || echo "   note: 无法设置 /Library/NullMoth 的读取权限（ARM 应用可能无法启动）"
if [ -f "$HERE/VERSION" ]; then mkdir -p /Library/NullMoth && tr -cd '0-9.\n' < "$HERE/VERSION" | head -n 1 > /Library/NullMoth/driver-version.tmp \
  && chmod 644 /Library/NullMoth/driver-version.tmp && mv /Library/NullMoth/driver-version.tmp /Library/NullMoth/driver-version; fi

step "6. 启动参数"
# A remove flag left by a boot-picker removal that did not finish keeps every NullMoth kext off at boot. Installing
# again means the driver is wanted, so the flag goes; the boot picker sets it again when it is chosen.
nvram -d 7C436110-AB2A-4BBB-A880-FE41995C9F82:nullmoth-remove 2>/dev/null || true

step "6b. 新驱动无法启动时恢复旧驱动"
# The kernel's boot guard (nvrm_off.h) turns the driver off after two starts in a row that never brought the display
# up, so the Mac reaches a desktop on the firmware's framebuffer. This one-shot job then puts the previous driver back
# from this install's backup and restarts once. It removes itself as soon as the new driver has shown its display
# (nullmoth-nv-ok), so nothing keeps running on a Mac where the update worked. A fresh install has no previous driver to
# go back to: the guard still keeps the Mac usable, and nothing is restored.
nvram -d 7C436110-AB2A-4BBB-A880-FE41995C9F82:nullmoth-nv-starts 2>/dev/null || true
nvram -d 7C436110-AB2A-4BBB-A880-FE41995C9F82:nullmoth-nv-ok 2>/dev/null || true
LG=/Library/NullMoth/lastgood.sh; LGP=/Library/LaunchDaemons/com.nullmoth.lastgood.plist
launchctl bootout system/com.nullmoth.lastgood 2>/dev/null || true
rm -f "$LG" "$LGP"
if [ -d "$BK/NVRM.kext" ]; then
  mkdir -p /Library/NullMoth /Library/LaunchDaemons || die "无法创建驱动回退任务所需的目录"
  cat > "$LG.tmp" <<EOF || die "无法写入驱动回退任务"
#!/bin/bash
# Written by the NVIDIA driver install: restores the driver in $BK if the installed one cannot start.
PATH=/usr/bin:/bin:/usr/sbin:/sbin
BK='$BK'; LOG=/Library/NullMoth/lastgood.log; V=7C436110-AB2A-4BBB-A880-FE41995C9F82
# no launchctl bootout here: booting out the running job kills this script before it can restart the Mac (10-17 test)
done_job() { rm -f /Library/LaunchDaemons/com.nullmoth.lastgood.plist /Library/NullMoth/lastgood.sh; }
if nvram \$V:nullmoth-nv-ok >/dev/null 2>&1; then echo "\$(date) the new driver brought its display up; keeping it" >> \$LOG; done_job; exit 0; fi
v=\$(nvram \$V:nullmoth-nv-starts 2>/dev/null | cut -f2)
case "\$v" in %0[2-9a-f]|%[1-9a-f]?) ;; *) exit 0;; esac
[ -d "\$BK/NVRM.kext" ] || { echo "\$(date) the driver turned itself off but its backup \$BK is gone" >> \$LOG; done_job; exit 0; }
echo "\$(date) the new driver did not bring the display up twice; restoring the previous driver from \$BK" >> \$LOG
for k in $CHANGED_KEXTS; do rm -rf "$EXT/\$k.kext"; [ ! -e "\$BK/\$k.kext" ] || ditto "\$BK/\$k.kext" "$EXT/\$k.kext"; done
for b in NVMTLDriver.bundle NVIDIAShared.bundle nvmtl NVKZinkGLDriver.bundle nvmtl-allow.txt; do rm -rf "$GB/\$b"; [ ! -e "\$BK/\$b" ] || ditto "\$BK/\$b" "$GB/\$b"; done
rm -rf /Library/NullMoth/kexts "$FW"
[ ! -d "\$BK/kexts" ] || ditto "\$BK/kexts" /Library/NullMoth/kexts
[ ! -d "\$BK/nvfw" ] || ditto "\$BK/nvfw" "$FW"
# the previous driver may be one that reads the old location
[ ! -d "\$BK/nvfw" ] || ! mkdir -m 0755 "$FWOLD" 2>/dev/null || ditto "\$BK/nvfw" "$FWOLD"
if [ -f "\$BK/AuxiliaryKernelExtensions.kc" ]; then cp -p "\$BK/AuxiliaryKernelExtensions.kc" "$KC"; else rm -f "$KC"; fi
[ ! -f "\$BK/os-major" ] || cp -p "\$BK/os-major" /Library/NullMoth/os-major
if [ -f "\$BK/driver-version" ]; then cp -p "\$BK/driver-version" /Library/NullMoth/driver-version; else rm -f /Library/NullMoth/driver-version; fi
if [ -f "\$BK/audio-touched" ]; then
  rm -f "$AUDIO_MARKER"
  [ ! -f "\$BK/nvidia-audio.sha256" ] || cp -p "\$BK/nvidia-audio.sha256" "$AUDIO_MARKER"
fi
nvram -d \$V:nullmoth-nv-starts 2>/dev/null
echo "previous" > /Library/NullMoth/driver-restored
done_job
echo "\$(date) previous driver restored; restarting" >> \$LOG
shutdown -r now
EOF
  chmod 755 "$LG.tmp" && chown root:wheel "$LG.tmp" && mv -f "$LG.tmp" "$LG" || die "无法安装驱动回退任务"
  cat > "$LGP" <<EOF || die "无法写入驱动回退任务的 plist"
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>Label</key><string>com.nullmoth.lastgood</string>
<key>ProgramArguments</key><array><string>/bin/bash</string><string>$LG</string></array>
<key>RunAtLoad</key><true/></dict></plist>
EOF
  chmod 644 "$LGP" && chown root:wheel "$LGP" || die "无法设置驱动回退任务的权限"
  ok "已将旧驱动保留为回退版本（${BK}）"
else
  ok "没有可回退的旧驱动（首次安装）"
fi
step "6c. 桌面启动后将本次启动标记为正常"
# The kernel's guard only counts a start as good when the framebuffer lights a head, or when the card reports that none
# of its connectors has a display. A laptop whose panel hangs off the integrated GPU reaches that report late and does
# not always reach it: two boots that missed it switched a healthy driver off, leaving the Mac with no NVIDIA card in
# Metal at all. A desktop that comes up and stays up is also a good start, so this job says so. A boot that never
# reaches a desktop keeps its count, which is what the guard is for.
BG=/Library/NullMoth/bootguard.sh; BGP=/Library/LaunchDaemons/com.nullmoth.bootguard.plist
launchctl bootout system/com.nullmoth.bootguard 2>/dev/null || true
mkdir -p /Library/NullMoth /Library/LaunchDaemons || die "无法创建启动保护任务所需的目录"
cat > "$BG.tmp" <<'EOF' || die "无法写入启动保护任务"
#!/bin/bash
# Written by the NVIDIA driver install: the desktop came up, so this start counts as good.
PATH=/usr/bin:/bin:/usr/sbin:/sbin
V=7C436110-AB2A-4BBB-A880-FE41995C9F82; LOG=/Library/NullMoth/bootguard.log
n=0
while [ "$n" -lt 48 ]; do
  if pgrep -x WindowServer >/dev/null 2>&1; then
    sleep 45
    pgrep -x WindowServer >/dev/null 2>&1 || { n=$((n + 1)); continue; }
    nvram -d $V:nullmoth-nv-starts 2>/dev/null
    nvram $V:nullmoth-nv-ok=1 2>/dev/null
    echo "$(date) the desktop came up and stayed up; this start counts as good" >> $LOG
    exit 0
  fi
  n=$((n + 1)); sleep 5
done
echo "$(date) no desktop within four minutes; the start keeps its count" >> $LOG
EOF
chmod 755 "$BG.tmp" && chown root:wheel "$BG.tmp" && mv -f "$BG.tmp" "$BG" || die "无法安装启动保护任务"
cat > "$BGP" <<EOF || die "无法写入启动保护任务的 plist"
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>Label</key><string>com.nullmoth.bootguard</string>
<key>ProgramArguments</key><array><string>/bin/bash</string><string>$BG</string></array>
<key>RunAtLoad</key><true/></dict></plist>
EOF
chmod 644 "$BGP" && chown root:wheel "$BGP" || die "无法设置启动保护任务的权限"
ok "桌面启动后将本次启动标记为正常"
# A missing boot-args variable is empty only after a successful NVRAM listing.
# A failed read must never become an empty value that we later overwrite.
read_boot_args() {
  local listing
  listing=$(nvram -p 2>/dev/null) || return 1
  printf '%s\n' "$listing" | sed -n $'s/^boot-args\t//p'
}
ba=$(read_boot_args) || ba=""
for a in nvfb=1 nvaccel=1; do case " $ba " in *" $a "*) ;; *) echo "   NOTE: 启动参数缺少 '$a'。请在 OpenCore config.plist 中添加（参见 README：手动安装）；如果另行安装了 1401 Mac App，也可用它设置 OpenCore。";; esac; done
# WindowServer is a platform binary: with AMFI enforcing it refuses the Metal plugin ("mapping process is a platform
# binary, but mapped file is not"), reaches the desktop with no Metal device and asserts "Failed to create MetalDevice".
# Two reports had boot-args with nvfb/nvaccel but neither AMFI argument.
amfi_note() {
  case " $1 " in *" amfi_get_out_of_my_way="*|*" amfi="*) ;;
    *) echo "   NOTE: 启动参数缺少 'amfi_get_out_of_my_way=0x1 amfi=0x80'。缺少这些参数时，WindowServer 会拒绝加载 NVIDIA Metal 插件，桌面将没有 Metal 设备。请在 OpenCore config.plist 中添加；如果另行安装了 1401 Mac App，也可用它设置 OpenCore。";;
  esac
}
amfi_note "$ba"
# WhateverGreen's boot screen copy (on unless gfxrst=4) reads a console mapping macOS may already have freed when this
# driver's display first opens, and the Mac panics in IOFramebuffer::postOpen.
# A note alone left machines unprotected, so the installer also adds gfxrst=4 to the NVRAM boot-args when no gfxrst is
# set (a gfxrst the user chose stays). OpenCore configs that write their own boot-args at every start replace the NVRAM
# value, so the note about config.plist stays; uninstall.sh removes only a gfxrst=4 recorded here as added.
weg_gfxrst() {   # $1 = current boot-args; prints the boot-args to write, or nothing when no change is due
  case " $1 " in *" gfxrst="*) return 0;; esac
  if [ -n "$1" ]; then echo "$1 gfxrst=4"; else echo "gfxrst=4"; fi
}
weg_gfxrst_apply() {
  local nb record temporary meta owner mode current
  current=$(read_boot_args) || return 1
  [ "$current" = "$1" ] || return 1
  nb=$(weg_gfxrst "$1"); [ -n "$nb" ] || return 0
  record=/Library/NullMoth/added-boot-args
  [ -d /Library/NullMoth ] && [ ! -L /Library/NullMoth ] || return 1
  meta=$(stat -f '%u %Lp' /Library/NullMoth) || return 1
  owner=${meta%% *}; mode=${meta#* }
  [ "$owner" = 0 ] || return 1
  case "$mode" in *[2367]?|*[2367]) return 1;; esac
  if [ -e "$record" ] || [ -L "$record" ]; then
    [ -f "$record" ] && [ ! -L "$record" ] && grep -qx 'gfxrst=4' "$record" || return 1
  fi
  temporary=$(mktemp /Library/NullMoth/.added-boot-args.XXXXXX) || return 1
  if ! (printf '%s\n' 'gfxrst=4' > "$temporary" && chmod 600 "$temporary" && chown root:wheel "$temporary"); then
    rm -f "$temporary"; return 1
  fi
  if ! nvram boot-args="$nb" 2>/dev/null; then rm -f "$temporary"; return 1; fi
  if ! mv -f "$temporary" "$record"; then
    # Do not replace another process's later boot-args while recovering a failed record write.
    if current=$(read_boot_args) && [ "$current" = "$nb" ]; then
      if [ -n "$1" ]; then nvram boot-args="$1"; else nvram -d boot-args; fi
    fi
    rm -f "$temporary"; return 1
  fi
  echo "   已加载 WhateverGreen：已将 gfxrst=4 添加到 NVRAM 启动参数（NVIDIA 显示器首次开启时，WhateverGreen 的启动画面副本可能导致内核崩溃）。"
}
if kmutil showloaded --list-only --bundle-identifier as.vit9696.WhateverGreen 2>/dev/null | grep -q WhateverGreen; then
  nb=$(weg_gfxrst "$ba")
  if [ -n "$nb" ]; then
    weg_gfxrst_apply "$ba" || echo "   NOTE: 无法连同恢复记录一起添加 gfxrst=4；重启前请检查启动参数。"
    echo "   NOTE: 还请在 OpenCore config.plist 启动参数中添加 'gfxrst=4'。每次启动都会设置启动参数的配置会覆盖 NVRAM 中的值。"
  fi
fi

# Commit ownership only after all install/link/job steps succeeded. Rollback and
# the boot fallback restore this record together with the matching binary.
if [ "$AUDIO_TOUCHED" = 1 ]; then
  if [ "$AUDIO" = 1 ]; then
    audio_sum=$(shasum -a 256 "$EXT/NMXAudio.kext/Contents/MacOS/NMXAudio") || die "无法计算已安装音频驱动的哈希值"
    audio_sum=${audio_sum%% *}
    printf '%s' "$audio_sum" | grep -Eq '^[0-9a-f]{64}$' || die "已安装音频驱动的哈希值无效"
    (umask 077; printf '%s\n' "$audio_sum" > "$AUDIO_MARKER.tmp") \
      && chown root:wheel "$AUDIO_MARKER.tmp" && chmod 600 "$AUDIO_MARKER.tmp" && mv -f "$AUDIO_MARKER.tmp" "$AUDIO_MARKER" \
      || die "无法记录音频启用状态"
  else rm -f "$AUDIO_MARKER" || die "无法移除音频启用状态记录"; fi
fi

# Native MetalFX DLSS is independent of the Wine/NVK provider.
choose_native_dlss() {
  printf '\n%s\n' '----------------------------------------------------------------------'
  echo "   可选的原生 MetalFX DLSS（RTX 20 系列及更新型号）。"
  printf '%s\n\n' '----------------------------------------------------------------------'
  echo "   供原生 Mac 游戏使用 MetalFX 超分辨率、降噪或帧生成。"
  echo "   将下载约 750 MB 的 NVIDIA 文件并编译 GPU 内核；需要几分钟时间和 2.5 GB 可用空间。"
  echo "   Wine/CrossOver 中的 DLSS 使用独立的 Wine/NVK 配置和游戏原版 nvngx_dlss.dll。"
  if [ "$DLSS_CHOICE" = ask ]; then
    if [ -t 0 ]; then
      printf '   是否安装或更新原生 MetalFX DLSS？[y/N] '
      answer=''; IFS= read -r answer || answer=''
      case "$answer" in y|Y|yes|YES) DLSS_CHOICE=yes;; *) DLSS_CHOICE=no;; esac
    else
      DLSS_CHOICE=no
    fi
  fi
  [ "$DLSS_CHOICE" = yes ]
}

# Optional DLSS 4 / 4.5 files: downloaded from NVIDIA on this Mac and checked against pinned hashes. A failure here
# never undoes the driver install.
# dlss_fetch.sh runs as root from the private copy; it is only trusted when SHA256SUMS covered it and its tools.
dlss_step_covered() {
  local entry
  # The fork's packager writes ./relative paths; upstream also uses bare paths.
  # Match the complete checksum record so another filename cannot cover a tool.
  for entry in 'dlss_fetch\.sh' 'dlss/dlss_cache' 'dlss/nm_unzstd' 'dlss/assertfail_trap\.ptx'; do
    grep -qE "^[0-9a-fA-F]{64} [ *](\./)?${entry}$" "$HERE/SHA256SUMS" || return 1
  done
}
if [ -f "$HERE/dlss_fetch.sh" ] && ! dlss_step_covered; then
  echo "   NOTE: 此软件包的 SHA256SUMS 未覆盖 DLSS 安装步骤，已跳过。"
elif [ -f "$HERE/dlss_fetch.sh" ]; then
  if choose_native_dlss; then
    step "原生 MetalFX DLSS 4 / 4.5 文件"
    /bin/bash "$HERE/dlss_fetch.sh" --yes || echo "   NOTE: DLSS 文件未能安装，但驱动已安装。稍后可重新运行：sudo ./install.sh --dlss=yes"
  else
    echo "   已跳过原生 MetalFX DLSS 的下载或更新；保留现有 DLSS 文件。Wine/CrossOver 配置不受影响。"
  fi
fi

# Optional WebKit helper lives in OpenCore, never the system kext repositories.
# This runs after the driver transaction. EFI failures retain the working driver.
webkit_manual() {
  echo "   Safari WebGL 辅助组件手动安装说明："
  printf '   kext 路径：%s/Optional/NVWebKitFixup/NVWebKitFixup.kext\n' "$SOURCE_PACKAGE"
  echo "   挂载 OpenCore EFI，并将该 kext 复制到 EFI/OC/Kexts/。保留已安装的 Lilu。"
  echo "   在 Kernel > Add 中紧接 Lilu 添加并启用 NVWebKitFixup："
  echo "   Arch=x86_64; ExecutablePath=Contents/MacOS/NVWebKitFixup; PlistPath=Contents/Info.plist"
  echo "   MinKernel=24.0.0；MaxKernel=25.99.99。先备份 EFI 并验证 config.plist，然后重启。"
  printf '   配置项模板：%s/Optional/NVWebKitFixup/Kernel-Add.plist\n' "$SOURCE_PACKAGE"
  printf '   完整指南和恢复步骤：%s/Optional/NVWebKitFixup/README.md\n' "$SOURCE_PACKAGE"
}
choose_webkit_fixup() {
  printf '\n%s\n' '----------------------------------------------------------------------'
  echo "   可选的 Safari WebGL 辅助组件，适用于 Intel Sequoia/Tahoe（需要 Lilu 1.7.2+）。"
  printf '%s\n\n' '----------------------------------------------------------------------'
  echo "   手动模式会显示复制和配置说明；自动模式会查找兼容的 OpenCore EFI，并先备份再更新。"
  echo "   如果找到多个候选项，请从列表中选择启动 EFI。--efi-target 可指定目标并覆盖自动检测。"
  if [ "$WEBKIT_CHOICE" = ask ]; then
    WEBKIT_CHOICE=manual
    if [ -t 0 ]; then
      printf '   Safari WebGL 辅助组件：[M]手动 / [a]自动 / [s]跳过 [M]：'
      answer=''; IFS= read -r answer || answer=''
      case "$answer" in a|A|auto|automatic) WEBKIT_CHOICE=auto;; s|S|skip) WEBKIT_CHOICE=skip;; esac
    fi
  fi
}
webkit_step_covered() {
  local entry
  for entry in 'efi\.py' 'Kernel-Add\.plist' 'NVWebKitFixup\.kext/Contents/Info\.plist' \
               'NVWebKitFixup\.kext/Contents/MacOS/NVWebKitFixup' 'NVWebKitFixup\.kext/Contents/_CodeSignature/CodeResources'; do
    grep -qE "^[0-9a-fA-F]{64} [ *](\./)?Optional/NVWebKitFixup/${entry}$" "$HERE/SHA256SUMS" || return 1
  done
}
if [ -d "$HERE/Optional/NVWebKitFixup/NVWebKitFixup.kext" ]; then
  choose_webkit_fixup
  case "$WEBKIT_CHOICE" in
    manual) webkit_manual;;
    auto)
      if ! webkit_step_covered; then
        echo "   NOTE: 软件包校验和未覆盖 EFI 辅助组件，已跳过自动安装。"
        webkit_manual
      else
        webkit_args=(install)
        [ -z "$EFI_TARGET" ] || webkit_args+=(--target "$EFI_TARGET")
        if ! /usr/bin/python3 "$HERE/Optional/NVWebKitFixup/efi.py" "${webkit_args[@]}"; then
          echo "   NOTE: EFI 辅助组件未能完成安装；NVIDIA 驱动已安装。请查看上方 EFI 备份和错误信息。"
          webkit_manual
        fi
      fi;;
    skip) echo "   已跳过 Safari WebGL 辅助组件；保留现有 EFI 项目。";;
  esac
fi

echo; echo "安装完成。现在重启：sudo shutdown -r now"
if [ "$ALLOWED" = "$(echo $KEXTS | wc -w | tr -d ' ')" ]; then
  echo "这是对已批准扩展的更新：重启一次即可。macOS 可能提示扩展已更新；新驱动会在本次重启时加载，无需再次批准。"
else
  echo "如果 macOS 提示，请在“系统设置”>“隐私与安全性”中允许这些扩展，然后再重启一次。"
fi
echo "撤销安装：sudo ./uninstall.sh $BK"
