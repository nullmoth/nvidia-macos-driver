# 1401 and NVIDIA driver 1.7.0

**Something not working? [nullmothsystems.com/help](https://nullmothsystems.com/help) lists every known problem and exactly what to do.**

## Windows (1401 1.7.0)
- **macOS gets your PC's ACPI table (DSDT) again on about 1 in 5 PCs.** On 43 of the 233 PCs whose tables users have sent (every Arrow Lake H laptop among them, and several AM5 boards), macOS was throwing the whole table away at startup. 1401 now repairs it in your EFI. Rebuild your EFI with 1.7 to get it.

## Driver / Mac
- When two OpenCore partitions are connected and neither config's serial number decides, setup now finds the one that started your Mac from this boot's own settings instead of stopping with "startup partition could not be confirmed".
- **Send logs now actually delivers the flicker capture and your OpenCore config.** Both were collected since 1.4/1.6 but lost a 12-file limit to other logs; they now go first.

## Scope
macOS 15 Sequoia. Requires OpenCore.
