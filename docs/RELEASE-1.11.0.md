# 1401 and NVIDIA driver 1.11.0

**Something not working? [nullmothsystems.com/help](https://nullmothsystems.com/help) lists every known problem and exactly what to do.**

## Mac app
- "AMFIPass.kext is not in this OpenCore EFI" no longer stops an install or update. Setup downloads Lilu and AMFIPass itself, from the same files 1401 builds with, checks each SHA-256 before changing anything, copies them into the EFI and adds them to the config.
- Firefox (and other apps that crashed at launch with AMFI relaxed) open: setup adds the `ipc_control_port_options=0` boot argument.
- A driver installed from the release `.tar.gz` records its version, so the app stops offering the version you already have.
- "STOP back up nvmtl" is fixed: the backup copies the Vulkan library links correctly.
- Installing a USB map no longer stops when USBToolBox is missing; setup adds it.
- When setup cannot tell which OpenCore partition started the Mac, it says why (OpenCore hides its boot path) and what to change (Misc > Security > ExposeSensitiveData 7), or choose the partition in the app. When it finds no OpenCore at all, it lists every partition it sees so the next report shows the cause.
- **Send logs** now includes the kernel log of the boots before this one, so a boot that hung or went black (which writes no panic) can be read, and panics macOS moved to its Retired folder.

## Windows (1401 1.11.0)
- Builds for a supported NVIDIA card include Lilu and AMFIPass, so the Mac app never has to ask for them.
- PCs stuck at the prohibited sign / `EB.MM.AKM`: a build after **Send logs** now reads the stick's last startup log too (it was moved into `NullMoth\sent-logs` and the build missed it), and sending that log tells you to build again. Starting an old stick again changes nothing; build it again with 1.11.
- Laptops with an Intel Iris Xe that macOS cannot drive no longer fail the build.
- A stick whose OpenCore EFI was not built by 1401 is named as the problem when its logs are sent.
- A failed stick write shows the real error instead of an internal one.

## Still open
- Screen flicker or a black screen above 60 Hz on some DisplayPort monitors. Use **Record the flicker** while it happens, then **Send logs**.
- WebGL in browsers, and some Apple Arcade games.
- HDMI and DisplayPort audio from the NVIDIA card.
- Laptops whose built-in screen is wired to the Intel graphics (no MUX switch).

## Scope
macOS 15 Sequoia. Requires OpenCore.
