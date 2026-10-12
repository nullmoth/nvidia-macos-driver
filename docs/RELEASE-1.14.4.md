# NullMoth NVIDIA driver and Mac app 1.14.4

**Something not working? [Why a machine does not work, and how to fix it](TROUBLESHOOTING.md) lists every known problem.**

A fix release for 1.14.3. Everything in [1.14.3](RELEASE-1.14.3.md) is included.

## Fixed
- **Full-screen flicker.** On machines where frames are copied to the screen instead of being shown straight from the
  GPU's memory, the copy sometimes took the picture before the GPU had finished drawing it, and a blank or stale frame
  went to the screen: up to every other frame on some 60 Hz and 240 Hz setups. The copy now waits for the finished
  picture, and if it is not ready the previous frame stays on screen.
- **Send logs now includes the AMD / Intel test driver.** Its messages were not collected before, so reports from
  machines that turned it on could not show what happened. If you tried it, send logs again with 1.14.4.

## Still open
- "Start once without NVIDIA driver" can leave the screen dark or frozen.
- Laptops whose built-in screen is wired to the Intel graphics (most laptops without a MUX switch): the screen works
  but without acceleration. Being worked on.
- HDMI and DisplayPort audio from the NVIDIA card; plugging in a new monitor while macOS runs.
