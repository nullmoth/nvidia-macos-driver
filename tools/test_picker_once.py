#!/usr/bin/env python3
"""Execute the production boot-picker plan against disposable configs with the real plutil: the start-once entry is
added, and a boot logo entry an earlier version added is removed with its file."""
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


def run(cfg, stage="install", efi_logo=True):
    s = SETUP.read_text()
    funcs = between(s, "has() {", "\nget() {", False) + "\n" + between(s, "get() {", "; }\n") + "\n"
    funcs += between(s, "tool_index() {", "logo_index() {", False) + between(s, "logo_index() {", "i=$((i+1)); done; }")
    names = between(s, 'TOOL_NAME="1401: Remove NVIDIA driver"', 'LOGO_FILE=NullMothLogo.efi\n')
    plan = between(s, "NEEDONCE=0; [ -z", 'ok "OpenCore already has every setting the driver needs"')
    apply = between(s, "  if [ $NEEDONCE = 1 ]; then", '    rm -f "$MP/$OCREL/Drivers/$LOGO_FILE" || fail=1\n  fi\n')
    remove = between(s, "      i=$(once_index); if [ -n", 'rm -f "$MP/$OCREL/Drivers/$LOGO_FILE"; fi')
    t = tempfile.mkdtemp()
    oc = os.path.join(t, "EFI", "OC")
    os.makedirs(os.path.join(oc, "Tools")); os.makedirs(os.path.join(oc, "Drivers"))
    c = os.path.join(oc, "config.plist")
    with open(c, "wb") as fh:
        plistlib.dump(cfg, fh)
    app = os.path.join(t, "app"); os.makedirs(app)
    pathlib.Path(app, "NullMothSafe.efi").write_bytes(b"tool")
    if efi_logo and any(d.get("Path") == "NullMothLogo.efi" for d in cfg.get("UEFI", {}).get("Drivers", [])):
        pathlib.Path(oc, "Drivers", "NullMothLogo.efi").write_bytes(b"logo")
    if any(x.get("Path") == "NullMothSafe.efi" for x in cfg.get("Misc", {}).get("Tools", [])):
        pathlib.Path(oc, "Tools", "NullMothSafe.efi").write_bytes(b"tool")
    edits = 'for e in "${EDITS[@]}"; do IFS="|" read -r k ty v <<<"$e"; plutil -replace "$k" $ty "$v" "$C"; done\n'
    body = remove if stage == "remove" else plan + "\n" + edits + apply
    script = (f'ok() {{ echo "OK $*"; }}\nstop() {{ echo "STOP $*"; exit 1; }}\nC="{c}"; MP="{t}"; OCREL=EFI/OC; TOOL="{app}/NullMothSafe.efi"\n'
              f'EDITS=(); DELADD=(); NEEDTOOL=0; NEEDBLOCK=0; NEEDAMFIPASS=0; fail=0\n{names}\n{funcs}\n{body}\n'
              'echo "FAIL=$fail"\n')
    r = subprocess.run(["/bin/bash", "-c", script], capture_output=True, text=True, timeout=60)
    assert r.returncode == 0, r.stdout + r.stderr
    with open(c, "rb") as fh:
        return r.stdout, plistlib.load(fh), t


BASE = {"Misc": {"Tools": [{"Path": "NullMothSafe.efi", "Arguments": "", "Name": "1401: Remove NVIDIA driver", "Enabled": True}]},
        "UEFI": {"Drivers": [{"Path": "OpenRuntime.efi", "Enabled": True, "LoadEarly": False, "Arguments": "", "Comment": "", "HideVerbose": False}]}}
LOGO = {"Path": "NullMothLogo.efi", "Enabled": True, "LoadEarly": False, "Arguments": "", "Comment": "NullMoth boot logo", "HideVerbose": False}


def with_logo():
    return {"Misc": dict(BASE["Misc"]), "UEFI": {"Drivers": BASE["UEFI"]["Drivers"] + [dict(LOGO)]}}


def has_logo(cfg):
    return any(d.get("Path") == "NullMothLogo.efi" for d in cfg["UEFI"]["Drivers"])


class PickerOnce(unittest.TestCase):
    def test_install_adds_the_once_entry_and_no_logo(self):
        out, cfg, folder = run(BASE)
        self.assertFalse(has_logo(cfg))
        self.assertTrue(any(x.get("Arguments") == "once" for x in cfg["Misc"]["Tools"]))
        self.assertEqual(len(cfg["Misc"]["Tools"]), 2)  # the remove entry was not mistaken for the once entry
        self.assertEqual(cfg["UEFI"]["Drivers"], BASE["UEFI"]["Drivers"])
        self.assertIn("FAIL=0", out)

    def test_a_second_run_changes_nothing(self):
        _, cfg, _ = run(BASE)
        out, cfg2, _ = run(cfg)
        self.assertEqual(cfg, cfg2)
        self.assertNotIn("CHANGE", out)

    def test_removing_the_driver_takes_the_once_entry(self):
        _, cfg, _ = run(BASE)
        _, after, _ = run(cfg, stage="remove")
        self.assertFalse([x for x in after["Misc"]["Tools"] if x.get("Arguments") == "once"])


class LegacyLogoRemoved(unittest.TestCase):
    """Earlier versions could add NullMothLogo.efi; an install, update or removal now takes it out, entry and file."""

    def test_an_update_removes_the_logo_entry_and_its_file(self):
        out, after, t = run(with_logo())
        self.assertFalse(has_logo(after))
        self.assertFalse(os.path.exists(os.path.join(t, "EFI/OC/Drivers/NullMothLogo.efi")))
        self.assertEqual(after["UEFI"]["Drivers"], BASE["UEFI"]["Drivers"])
        self.assertIn("CHANGE boot logo: remove", out)

    def test_an_entry_whose_file_is_already_gone_is_removed_too(self):
        _, after, _ = run(with_logo(), efi_logo=False)
        self.assertFalse(has_logo(after))

    def test_removing_the_driver_removes_the_logo_too(self):
        _, after, t = run(with_logo(), stage="remove")
        self.assertFalse(has_logo(after))
        self.assertFalse(os.path.exists(os.path.join(t, "EFI/OC/Drivers/NullMothLogo.efi")))
        self.assertEqual(after["UEFI"]["Drivers"], BASE["UEFI"]["Drivers"])


if __name__ == "__main__":
    unittest.main()
