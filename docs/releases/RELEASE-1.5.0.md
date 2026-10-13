# 1401 and NVIDIA driver 1.5.0

**Something not working? [nullmothsystems.com/help](https://nullmothsystems.com/help) lists every known problem and exactly what to do.**

## Windows (1401 1.5.0)
- **The macOS download finishes on networks that cut it off.** On some networks the first connection got exactly 1 MB and every retry got nothing ("after 9 resumed connections, 8 in a row with no progress"). 1401 now fetches the rest in small pieces that get under the limit, waits longer between failed tries, and still checks every piece against Apple's signed list.

## Driver / Mac
- Same driver as 1.4.0 (flicker cleared at login, display capture for Send logs, RTX 50 fix, hot-plug, Blender, Geekbench).

## Ryzen 7000 / 9000 (AM5)
- If the stick stops with `EB.MM.AKM` / "Couldn't allocate", restart and pick the installer again 2–3 times before rebuilding — on AM5 this is often random from one start to the next.

## Scope
macOS 15 Sequoia. Requires OpenCore.
