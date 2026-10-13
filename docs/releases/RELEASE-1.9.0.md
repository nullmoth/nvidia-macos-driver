# 1401 and NVIDIA driver 1.9.0

**Something not working? [nullmothsystems.com/help](https://nullmothsystems.com/help) lists every known problem and exactly what to do.**

## Windows (1401 1.9.0)
- Ryzen (AM5) boards stuck at the prohibited sign / `EB.MM.AKM`: one more memory setting is tried before giving up.
- Sticks that refuse direct writes, and sticks Windows doesn't show after the erase, are erased with diskpart instead of stopping.
- Apple's recovery server errors (502, timeouts, resets) are retried longer.

## Driver / Mac
- Same driver as 1.7.0.

## Scope
macOS 15 Sequoia. Requires OpenCore.
