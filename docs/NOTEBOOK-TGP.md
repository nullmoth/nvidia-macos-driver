# Optional notebook power budget

`nvrmtgp=1` enables the experimental notebook TGP manager. It is **off by
default**. A detected laptop battery is required, and the firmware helper
refuses non-mobile GPUs. Desktop installations need no additional argument.
The manager also stays inactive with `-nvrmnopm`, which disables its sleep hooks.

The manager asks NVIDIA firmware to enable the laptop's configured TGP budget
while on mains power. Wattage remains a firmware/platform decision: a reported
140 W result on one laptop does not establish another model's budget. This
implements the TGP-mode step, not the complete CPU/GPU Dynamic Boost policy.

Add `nvrmtgp=1` to the existing OpenCore `boot-args` described in
[OpenCore setup](OPENCORE-SETUP.md), then reboot into a driver containing this
change. The installer does not add it automatically. The first check is two
seconds after adapter bring-up; subsequent checks run every five seconds.
Removing the charger resets TGP mode. A missing power-source reading also
resets an applied budget and cannot enable one.

Inspect the manager with `nvmtl-smi` or:

```sh
sysctl debug.nvrm_tgp_enable debug.nvrm_tgp_state debug.nvrm_tgp_ac \
  debug.nvrm_tgp_last_status debug.nvrm_tgp_applies
```

`state` is the last successful command: `1` enabled, `0` reset, `-1` none
since startup/resume. `ac` is `1` mains, `0` battery or `-1` unknown.
`last_status` records the firmware/control error and `applies` counts attempted
controls, including failures. These are command results, not measured watts.
The existing telemetry interface covers one opted-in adapter.

To reset the budget while running:

```sh
sudo sysctl debug.nvrm_tgp_enable=0
```

The next poll attempts the reset, even if the power-source reading is unknown.
Setting it to `1` resumes following the charger. Five consecutive refused
controls suppress further enable attempts until successful GPU resume;
attempts to reset an applied budget remain available. Check `last_status`:
a refused reset is not proof that the firmware has lowered its limit.
Remove `nvrmtgp=1` and reboot to disable the manager persistently.

Sleep drains the current poll before RM standby. Only a successful RM/NVKMS
resume re-arms it with fresh firmware state. Driver stop drains the timer and
attempts to reset an active budget before adapter teardown. Stopped managers
cannot be reactivated by a late callback or resume.

The control is adapted from upstream
[`dfe54e0`](https://github.com/nullmoth/nvidia-macos-driver/commit/dfe54e0329308a8acd8de5e1246354454fcb557a),
with gated timer lifetime, mobile-GPU checks and conservative power-source
handling. Host tests exercise the production manager and RM helper; a private
NVRM build and isolated Tahoe kernel-collection link verify build compatibility.
Laptop charger/battery transitions, enforced wattage, thermal behavior and
sleep/resume still require testing on affected hardware. No laptop power gain
has been measured for this fork.
