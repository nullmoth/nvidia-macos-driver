# 1401 and NVIDIA driver 1.8.0

**Something not working? [nullmothsystems.com/help](https://nullmothsystems.com/help) lists every known problem and exactly what to do.**

## Windows (1401 1.8.0)
- **Machines we've already solved build right the first time.** A board and CPU that match a machine whose own boots showed what it needs get those settings in the first EFI 1401 builds.

- "Format-Volume: Invalid property" / "format.com: Required parameter missing" and "WinError 2" at the erase step fixed.

## Driver / Mac
- **"Startup partition could not be confirmed" with only one OpenCore partition** fixed: when OpenCore started your Mac and only one partition holds OpenCore, setup uses it.
- Same driver as 1.7.0.

## Scope
macOS 15 Sequoia. Requires OpenCore.
