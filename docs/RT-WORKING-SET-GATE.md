# Ray tracing working-set gate: measurement procedure

Some games decide whether to offer ray tracing from the working-set figure the Metal device reports
(`recommendedMaxWorkingSetSize`). The plugin keeps a knob for this, `NVMTL_WS_RT_GATE_MB`, default off. Its default for
the one game profile that sets it (`plugin/nvmtl_app_profile.h`) was found by the black-box procedure below, and this
page is how to re-measure it on any card without reading the game.

## What is measured

The smallest reported working set, in MB, at which the game's graphics menu offers its ray tracing options, with the
rest of the system unchanged (same driver, same card, same game build).

## Procedure (one person, one card, about 20 minutes)

1. Install the driver build under test and confirm ray tracing is reported available:
   `grep -i "ray" "$TMPDIR/nvmtl.log"` shows the device answered `supportsRaytracing = YES`.
2. Start the game from a shell with `NVMTL_WS_RT_GATE_MB=<n>` set. Use a starting value well below the card's own
   figure so the knob does nothing (the knob only raises the answer, it never lowers it), then raise it.
3. Open the graphics menu and note whether the ray tracing options are present and selectable.
4. Bisect `<n>` between a value that hides the options and a value that shows them until the interval is 16 MB wide.
   Restart the game for every value: the game reads the figure once at device set-up.
5. Record the pair (last value hidden, first value shown), the card, and the driver build.
6. Check the same value against the low-memory behaviour: with `<n>` just below the threshold, texture quality
   options that need more memory are expected to be limited; with `<n>` above, they are not.
7. A card with less than 1 GB of memory above `<n>` keeps its own answer (the plugin refuses to over-report). On such a
   card the procedure ends at step 2 with "refused" in the log: that is the expected result, not a failure.

## Pass criteria

- The threshold found in step 4 is within 16 MB of the profile default. If it differs, change the default and
  record the measurement here.
- With the knob unset, behaviour is identical to a build without the knob.

## Measurements

| date | card | driver build | hidden at | shown at |
|---|---|---|---|---|
| to be recorded on the integration card | | | | |
