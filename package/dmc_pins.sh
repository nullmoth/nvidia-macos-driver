# Pins for the Intel display firmware (package/dmc_fetch.sh, package/nm-dmc-load.sh): one commit of linux-firmware, one SHA-256 per file.
COMMIT=a42217fc9c813dfc2e0254f0ac21ff6034098a79
BASE="https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git/plain/i915"
FILES="tgl_dmc_ver2_12.bin adlp_dmc_ver2_16.bin mtl_dmc.bin"
dmc_sha() {
  case "$1" in
    tgl_dmc_ver2_12.bin)  echo 3c013ef0ad96ba73aee8e5bd04a8e27cc9b1c6e9183b1a83ce124485f325afca;;
    adlp_dmc_ver2_16.bin) echo 2da482ea46a40e54c9ca3b54185959177f393eff98ece21acdac7eb6cacb0fcb;;
    mtl_dmc.bin)          echo 1b3b826195d50fae6611d29e0eb348e641a3ada3e0436ee0b987124f47a0e0db;;
    *) echo none;;
  esac
}
