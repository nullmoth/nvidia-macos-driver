# vtinternals: tools used to find out how VideoToolbox names and loads encoders (T7.00, 2026-10-18)

Run in the macOS guest (clang; no Xcode project). They read Apple's code in-process; nothing here ships.

- `fnmap.c` - outline an x86_64 function: direct calls, GOT calls and stubs resolved with dladdr, CFString/C-string
  references. `./fnmap SYMBOL [BYTES] [-r]`, `FNMAP_OFF=n`, `FNMAP_ADDR=0x…`.
- `vtreg.c` - list VideoToolbox's own encoder FigRegistry (pointer found from the search-path code on 26.7.1), its search
  paths and per-path caches, an item's description dictionary; `RENAME=1` renames our H.264 entry in place.
- `regtest.c` - a private FigRegistry over the plug-in folders with options 0 (no Apple signature check).
- `bundletest.m`, `byid.m` - VTRegisterVideoEncoderBundleDirectory and encoder-by-ID tests; `probe.c` + `Info.plist` -
  a dummy encoder plug-in bundle that records when its factory is called.
