# NVRMFB backlight power requests

`linear-brightness` and `brightness` requests from the display slider update the saved user target. Whole-display power requests from AppleBacklightDisplay do not update that target; a temporary request can dim the panel further, but a request above the user's target is capped at the saved level. Requests still run through the existing asynchronous NVKMS call, and the existing 5% hardware floor remains in effect.

The production check is `python3 tools/check_nvrmfb_backlight_target.py`. It extracts and compiles `NVRMBacklightHandler::doIntegerSet` from the driver source against a small fixture. It does not build the kext or replace the submitter's physical validation: the RTX 5080 Laptop/macOS 26.7 result, including screen appearance, lid close/open, and persistence across restart, was reported by the submitter.
