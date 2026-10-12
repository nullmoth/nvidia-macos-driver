#!/usr/bin/env python3
"""Execute the production logo/picker plan against disposable configs with the real plutil."""
import os
import pathlib
import plistlib
import subprocess
import tempfile
import unittest

SETUP = pathlib.Path(__file__).resolve().parents[1] / "app/Resources/nullmoth-setup.sh"


def between(s, start, end, include_end=True):
    a = s.index(start)
    b = s.index(end, a) + (len(end) if include_end else 0)
    return s[a:b]


def run(cfg, logo=None, stage="install"):
    s = SETUP.read_text()
    funcs = between(s, "has() {", "\nget() {", False) + "\n" + between(s, "get() {", "; }\n") + "\n"
    funcs += between(s, "tool_index() {", "logo_index() {", False) + between(s, "logo_index() {", "i=$((i+1)); done; }")
    names = between(s, 'TOOL_NAME="1401: Remove NVIDIA driver"', 'LOGO=off; LOGO_FILE=NullMothLogo.efi\n')
    plan = between(s, "NEEDONCE=0; [ -z", 'ok "OpenCore already has every setting the driver needs"')
    apply = between(s, "  if [ $NEEDONCE = 1 ]; then", '  [ "${REFRESHLOGO:-0}" = 1 ] && { cp "$LOGOSRC" "$MP/$OCREL/Drivers/$LOGO_FILE" || fail=1; }')
    remove = between(s, "      i=$(once_index); if [ -n", 'nothing changed"; fi')
    t = tempfile.mkdtemp()
    oc = os.path.join(t, "EFI", "OC")
    os.makedirs(os.path.join(oc, "Tools")); os.makedirs(os.path.join(oc, "Drivers"))
    c = os.path.join(oc, "config.plist")
    if not os.path.exists(c):
        with open(c, "wb") as fh:
            plistlib.dump(cfg, fh)
    app = os.path.join(t, "app"); os.makedirs(app)
    for n, b in (("NullMothSafe.efi", b"tool"), ("NullMothLogo.efi", b"logo")):
        pathlib.Path(app, n).write_bytes(b)
    # a config from an earlier run comes with the EFI files that run copied (each run gets a fresh folder here)
    if any(d.get("Path") == "NullMothLogo.efi" for d in cfg.get("UEFI", {}).get("Drivers", [])):
        pathlib.Path(oc, "Drivers", "NullMothLogo.efi").write_bytes(b"logo")
    if any(x.get("Path") == "NullMothSafe.efi" for x in cfg.get("Misc", {}).get("Tools", [])):
        pathlib.Path(oc, "Tools", "NullMothSafe.efi").write_bytes(b"tool")
    edits = 'for e in "${EDITS[@]}"; do IFS="|" read -r k ty v <<<"$e"; plutil -replace "$k" $ty "$v" "$C"; done\n'
    body = remove if stage == "remove" else plan + "\n" + edits + apply
    script = (f'ok() {{ echo "OK $*"; }}\nstop() {{ echo "STOP $*"; exit 1; }}\nC="{c}"; MP="{t}"; OCREL=EFI/OC; TOOL="{app}/NullMothSafe.efi"\n'
              f'EDITS=(); DELADD=(); NEEDTOOL=0; NEEDBLOCK=0; NEEDAMFIPASS=0; fail=0\n{names}\n' + (f'LOGO={logo}\n' if logo is not None else '') + f'{funcs}\n{body}\n'
              'echo "FAIL=$fail"\n')
    r = subprocess.run(["/bin/bash", "-c", script], capture_output=True, text=True, timeout=60)
    assert r.returncode == 0, r.stdout + r.stderr
    with open(c, "rb") as fh:
        return r.stdout, plistlib.load(fh), t


BASE = {"Misc": {"Tools": [{"Path": "NullMothSafe.efi", "Arguments": "", "Name": "1401: Remove NVIDIA driver", "Enabled": True}]},
        "UEFI": {"Drivers": [{"Path": "OpenRuntime.efi", "Enabled": True, "LoadEarly": False, "Arguments": "", "Comment": "", "HideVerbose": False}]}}


class LogoAndOnce(unittest.TestCase):
    def test_default_does_not_add_a_cosmetic_driver(self):
        out, cfg, folder = run(BASE)
        self.assertFalse(any(d.get("Path") == "NullMothLogo.efi" for d in cfg["UEFI"]["Drivers"]))
        self.assertFalse(os.path.exists(os.path.join(folder, "EFI/OC/Drivers/NullMothLogo.efi")))
        self.assertTrue(any(x.get("Arguments") == "once" for x in cfg["Misc"]["Tools"]))
        self.assertEqual(cfg["UEFI"]["Drivers"], BASE["UEFI"]["Drivers"])

    def test_default_update_disables_only_the_existing_logo(self):
        _, cfg, _ = run(BASE, logo="on")
        out, updated, _ = run(cfg)
        self.assertFalse(next(d for d in updated["UEFI"]["Drivers"] if d["Path"] == "NullMothLogo.efi")["Enabled"])
        self.assertEqual(updated["UEFI"]["Drivers"][0], BASE["UEFI"]["Drivers"][0])
        self.assertEqual(updated["Misc"], cfg["Misc"])
        _, repeated, _ = run(updated)
        self.assertEqual(repeated, updated)

    def test_explicit_opt_in_adds_the_logo_with_the_configs_keys_and_the_once_entry(self):
        out, cfg, t = run(BASE, logo="on")
        logo = [d for d in cfg["UEFI"]["Drivers"] if d["Path"] == "NullMothLogo.efi"]
        once = [x for x in cfg["Misc"]["Tools"] if x.get("Arguments") == "once"]
        self.assertEqual(len(logo), 1)
        self.assertTrue(logo[0]["Enabled"])
        self.assertIn("HideVerbose", logo[0])
        self.assertEqual(len(once), 1)
        self.assertEqual(len(cfg["Misc"]["Tools"]), 2)  # the remove entry was not mistaken for the once entry
        self.assertTrue(os.path.isfile(os.path.join(t, "EFI/OC/Drivers/NullMothLogo.efi")))
        self.assertIn("FAIL=0", out)

    def test_a_second_run_changes_nothing(self):
        _, cfg, _ = run(BASE, logo="on")
        out, cfg2, _ = run(cfg, logo="on")
        self.assertEqual(cfg, cfg2)
        self.assertNotIn("CHANGE", out)

    def test_logo_off_turns_it_off_and_keeps_the_entry(self):
        _, cfg, _ = run(BASE, logo="on")
        out, cfg2, _ = run(cfg, logo="off")
        logo = [d for d in cfg2["UEFI"]["Drivers"] if d["Path"] == "NullMothLogo.efi"]
        self.assertEqual(len(logo), 1)
        self.assertFalse(logo[0]["Enabled"])
        self.assertIn("CHANGE boot logo: turn the NullMoth boot logo off", out)

    def test_an_older_opencore_without_hideverbose_gets_no_hideverbose(self):
        old = {"Misc": {"Tools": []}, "UEFI": {"Drivers": [{"Path": "OpenRuntime.efi", "Enabled": True, "LoadEarly": False, "Arguments": ""}]}}
        _, cfg, _ = run(old, logo="on")
        logo = [d for d in cfg["UEFI"]["Drivers"] if d["Path"] == "NullMothLogo.efi"][0]
        self.assertNotIn("HideVerbose", logo)

    def test_removing_the_driver_takes_the_once_entry_and_keeps_the_logo(self):
        _, cfg, _ = run(BASE, logo="on")
        _, after, _ = run(cfg, stage="remove")
        self.assertFalse([x for x in after["Misc"]["Tools"] if x.get("Arguments") == "once"])
        self.assertTrue([d for d in after["UEFI"]["Drivers"] if d["Path"] == "NullMothLogo.efi"])


if __name__ == "__main__":
    unittest.main()
