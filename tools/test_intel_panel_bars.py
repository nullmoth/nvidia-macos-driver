#!/usr/bin/env python3
"""A laptop whose built-in panel is on an Intel GPU macOS has no driver for keeps the installer's BAR settings
(ResizeGpuBars -1, ResizeAppleGpuBars 0): issue #119 (Inspiron 16 7610, Tiger Lake-H + RTX 3050 Laptop, no MUX) had nothing
usable on the panel with ResizeAppleGpuBars -1. A desktop that only left its iGPU on, monitor on the NVIDIA card, keeps
the 8 GB BAR. Runs the setup's real main-flow step and the update's finish step on stub registries and temp configs."""
import os
import pathlib
import plistlib
import subprocess
import tempfile
import unittest

SETUP = pathlib.Path(__file__).resolve().parents[1] / "app/Resources/nullmoth-setup.sh"
HELPERS = ("bidx() {", "\n# Copy newest files")
MAIN = ("# The macOS installer boots with a small GPU BAR", "# AMD (AMD Vanilla patches)")
FINISH = ("    # The BAR and IONDRVSupport settings setup itself writes", "    args_set del -nvoff")
SETQ = ("  setq() {", "\n")
IGPU = "PciRoot(0x0)/Pci(0x2,0x0)"
IONDRV = "com.apple.iokit.IONDRVSupport"


def cut(s, start, end):
    assert s.count(start) == 1, "anchor moved in nullmoth-setup.sh: " + start
    i = s.index(start)
    return s[i:s.index(end, i)]


def le32(v):
    return v.to_bytes(4, "little").hex()


def pci(*devs):
    """ioreg -r -c IOPCIDevice -l: (name, vendor, device, class code) per function."""
    return "".join(f'+-o {n}  <class IOPCIDevice, id 0x10000{i:04x}, registered, matched, active, busy 0 (0 ms), retain 9>\n'
                   f'  | {{\n  |   "vendor-id" = <{le32(v)}>\n  |   "device-id" = <{le32(d)}>\n  |   "class-code" = <{le32(c)}>\n  | }}\n'
                   for i, (n, v, d, c) in enumerate(devs))


def acpi(*devs):
    """ioreg -r -c IOACPIPlatformDevice -l: (name, _HID) per device."""
    return "".join(f'+-o {n}  <class IOACPIPlatformDevice, id 0x10001{i:04x}, registered, matched, active, busy 0 (0 ms), retain 8>\n'
                   f'  | {{\n  |   "name" = <"{h}">\n  | }}\n' for i, (n, h) in enumerate(devs))


def acpi_compatible(name, hid, *compatible):
    """One ACPI device whose _HID is neither lid nor battery but whose compatible list holds one: <"A","B">."""
    ids = ",".join(f'"{c}"' for c in compatible)
    return (f'+-o {name}  <class IOACPIPlatformDevice, id 0x100020000, registered, matched, active, busy 0 (0 ms), retain 8>\n'
            f'  | {{\n  |   "name" = <"{hid}">\n  |   "compatible" = <{ids}>\n  | }}\n')


VGA, D3D, OTHER, AUDIO = 0x030000, 0x030200, 0x038000, 0x040300
LAPTOP_ACPI = acpi(("PWRB", "PNP0C0C"), ("LID0", "PNP0C0D"), ("BAT0", "PNP0C0A"), ("ADP1", "ACPI0003"))
DESKTOP_ACPI = acpi(("PWRB", "PNP0C0C"), ("SLPB", "PNP0C0E"), ("PCI0", "PNP0A08"))
# Issue #119: the panel and HDMI on the Intel GPU, the GA107M a 3D controller with no connectors.
INSPIRON_119 = (pci(("IGPU@2", 0x8086, 0x9A60, VGA), ("GFX0@0", 0x10DE, 0x25A2, D3D), ("HDAU@0,1", 0x10DE, 0x2291, AUDIO)), LAPTOP_ACPI)
# Panel on the Intel GPU, HDMI on the NVIDIA card (so it is a VGA controller).
LAPTOP_HDMI_ON_NVIDIA = (pci(("IGPU@2", 0x8086, 0x9A49, VGA), ("GFX0@0", 0x10DE, 0x2520, VGA)), LAPTOP_ACPI)
# An NVIDIA GPU with no outputs and no lid or battery device (an all-in-one, or firmware that hides them).
HEADLESS_NVIDIA_NO_LID = (pci(("IGPU@2", 0x8086, 0x9A60, VGA), ("GFX0@0", 0x10DE, 0x25A2, D3D)), DESKTOP_ACPI)
NVIDIA_DESKTOP = (pci(("GFX0@0", 0x10DE, 0x2D05, VGA), ("HDAU@0,1", 0x10DE, 0x22EB, AUDIO)), DESKTOP_ACPI)
# A laptop is told by its ACPI lid or battery alone, also when either is only in a "compatible" list.
BATTERY_ONLY = acpi(("PWRB", "PNP0C0C"), ("BAT0", "PNP0C0A"))
LID_ONLY = acpi(("PWRB", "PNP0C0C"), ("LID0", "PNP0C0D"))
LID_IN_COMPATIBLE = DESKTOP_ACPI + acpi_compatible("LID0", "LEN0068", "MSFT0001", "PNP0C0D")
# NVIDIA display-class 0380 has no outputs either.
HEADLESS_NVIDIA_0380 = (pci(("IGPU@2", 0x8086, 0x9A60, VGA), ("GFX0@0", 0x10DE, 0x25A2, OTHER)), DESKTOP_ACPI)
# A desktop with an NVIDIA display card and an NVIDIA compute card (no outputs), iGPU left on: the display card decides.
DESKTOP_DISPLAY_AND_COMPUTE = (pci(("IGPU@2", 0x8086, 0x4680, VGA), ("GFX0@0", 0x10DE, 0x2D05, VGA),
                                   ("GFX1@0", 0x10DE, 0x20B5, D3D)), DESKTOP_ACPI)
# An Intel iGPU that is not the display controller (class 0380, the card made primary) is no firmware panel.
DESKTOP_INTEL_0380 = (pci(("IGPU@2", 0x8086, 0x4680, OTHER), ("GFX0@0", 0x10DE, 0x2D05, VGA)), DESKTOP_ACPI)
# The design concern: Alder Lake desktop, iGPU left on in the firmware with no properties, monitor on the NVIDIA card.
DESKTOP_IGPU_ON = (pci(("IGPU@2", 0x8086, 0x4680, VGA), ("GFX0@0", 0x10DE, 0x2D05, VGA)), DESKTOP_ACPI)


def config(bar, abar, block=None, igpu_props=None):
    c = {"UEFI": {"Quirks": {"ResizeGpuBars": bar}}, "Booter": {"Quirks": {"ResizeAppleGpuBars": abar}}, "Kernel": {"Block": []}}
    if block is not None:
        c["Kernel"]["Block"].append({"Identifier": IONDRV, "Enabled": block, "Strategy": "Exclude"})
    if igpu_props:
        c["DeviceProperties"] = {"Add": {IGPU: igpu_props}}
    return c


def run(machine, cfg, body):
    s = SETUP.read_text()
    t = tempfile.mkdtemp()
    paths = {}
    for k, text in zip(("pci", "acpi", "cfg"), (*machine, None)):
        paths[k] = os.path.join(t, k)
        if text is not None:
            pathlib.Path(paths[k]).write_text(text)
    with open(paths["cfg"], "wb") as f:
        plistlib.dump(cfg, f)
    script = (f'C="{paths["cfg"]}"; EDITS=()\n'
              'has() { plutil -extract "$1" raw -o - "$C" >/dev/null 2>&1; }\n'
              'get() { plutil -extract "$1" raw -o - "$C" 2>/dev/null; }\n'
              'note() { echo "NOTE $*"; }\n'
              f'ioreg() {{ case "$*" in *IOPCIDevice*) cat "{paths["pci"]}";; *IOACPIPlatformDevice*) cat "{paths["acpi"]}";; *) return 1;; esac; }}\n'
              + cut(s, *HELPERS) + "\n" + cut(s, *SETQ) + "\n" + body(s) + "\n"
              'for e in "${EDITS[@]}"; do echo "EDIT $e"; done; echo "NEEDBLOCK ${NEEDBLOCK:-}"; echo "PANEL $INTELPANEL $PANELBARS"\n')
    r = subprocess.run(["/bin/bash", "-c", script], capture_output=True, text=True, timeout=60)
    assert r.returncode == 0, r.stdout + r.stderr
    with open(paths["cfg"], "rb") as f:
        return r.stdout.splitlines(), plistlib.load(f)


def main_flow(machine, cfg):
    out, _ = run(machine, cfg, lambda s: cut(s, *MAIN))
    return out


def finish(machine, cfg):
    _, c = run(machine, cfg, lambda s: cut(s, *FINISH))
    return bars_and_block(c)


def bars_and_block(c):
    blocks = [b["Enabled"] for b in c["Kernel"]["Block"] if b["Identifier"] == IONDRV]
    return c["UEFI"]["Quirks"]["ResizeGpuBars"], c["Booter"]["Quirks"]["ResizeAppleGpuBars"], blocks[0] if blocks else None


def edits(out):
    return [l[5:] for l in out if l.startswith("EDIT ")]


def after_setup(machine, cfg):
    """What setup's main flow leaves in the config: its EDITS applied, plus the exclude entry when NEEDBLOCK is 1."""
    out = main_flow(machine, cfg)
    bar, abar, block = bars_and_block(cfg)
    for e in edits(out):
        k, _, v = e.split("|")
        if k == "UEFI.Quirks.ResizeGpuBars": bar = int(v)
        elif k == "Booter.Quirks.ResizeAppleGpuBars": abar = int(v)
        elif k.startswith("Kernel.Block."): block = v == "true"
    if "NEEDBLOCK 1" in out: block = True
    return bar, abar, block


PREPARED = dict(bar=-1, abar=0, block=False)   # what the update's prepare step leaves behind


class MainFlow(unittest.TestCase):
    def test_intel_panel_laptop_gets_the_installer_bars_and_keeps_iondrvsupport(self):
        out = main_flow(INSPIRON_119, config(13, -1, block=True))
        self.assertEqual(edits(out), ["UEFI.Quirks.ResizeGpuBars|-integer|-1", "Booter.Quirks.ResizeAppleGpuBars|-integer|0",
                                      "Kernel.Block.0.Enabled|-bool|false"])
        self.assertIn("NEEDBLOCK 0", out)
        changes = [l for l in out if l.startswith("CHANGE Resize")]
        self.assertEqual(len(changes), 2)
        self.assertTrue(all("#119" in l for l in changes), changes)

    def test_intel_panel_laptop_already_on_the_installer_bars_changes_nothing(self):
        out = main_flow(INSPIRON_119, config(-1, 0))
        self.assertEqual(edits(out), [])
        self.assertIn("NEEDBLOCK 0", out)

    def test_laptop_with_hdmi_on_the_nvidia_card_still_keeps_its_panel(self):
        self.assertEqual(after_setup(LAPTOP_HDMI_ON_NVIDIA, config(13, -1)), (-1, 0, None))

    def test_nvidia_without_outputs_is_enough_without_a_lid(self):
        out = main_flow(HEADLESS_NVIDIA_NO_LID, config(13, -1))
        self.assertIn("PANEL 1 1", out)
        self.assertTrue(any("no display outputs" in l for l in out if l.startswith("NOTE")), out)

    def test_lid_or_battery_alone_marks_a_laptop(self):
        for name, a in [("battery only", BATTERY_ONLY), ("lid only", LID_ONLY), ("lid in a compatible list", LID_IN_COMPATIBLE)]:
            with self.subTest(name):
                self.assertIn("PANEL 1 1", main_flow((LAPTOP_HDMI_ON_NVIDIA[0], a), config(13, -1)))

    def test_nvidia_class_0380_has_no_outputs(self):
        self.assertIn("PANEL 1 1", main_flow(HEADLESS_NVIDIA_0380, config(13, -1)))

    def test_an_nvidia_display_card_outweighs_a_compute_card(self):
        out = main_flow(DESKTOP_DISPLAY_AND_COMPUTE, config(-1, 0, block=False))
        self.assertIn("PANEL 1 0", out)
        self.assertEqual(edits(out), ["UEFI.Quirks.ResizeGpuBars|-integer|13", "Booter.Quirks.ResizeAppleGpuBars|-integer|-1"])

    def test_intel_gpu_that_is_not_the_display_controller_is_not_a_panel(self):
        out = main_flow(DESKTOP_INTEL_0380, config(-1, 0))
        self.assertIn("PANEL 0 0", out)
        self.assertIn("NEEDBLOCK 1", out)

    def test_nvidia_only_desktop_gets_the_full_bar_and_the_exclude(self):
        out = main_flow(NVIDIA_DESKTOP, config(-1, 0))
        self.assertEqual(edits(out), ["UEFI.Quirks.ResizeGpuBars|-integer|13", "Booter.Quirks.ResizeAppleGpuBars|-integer|-1"])
        self.assertIn("NEEDBLOCK 1", out)
        self.assertIn("PANEL 0 0", out)

    def test_desktop_with_igpu_on_and_monitor_on_nvidia_keeps_the_full_bar(self):
        out = main_flow(DESKTOP_IGPU_ON, config(-1, 0, block=False))
        # the full BAR, as on the RTX 5060 it was measured on; IONDRVSupport stays on as 1.12 decided for this GPU
        self.assertEqual(edits(out), ["UEFI.Quirks.ResizeGpuBars|-integer|13", "Booter.Quirks.ResizeAppleGpuBars|-integer|-1"])
        self.assertIn("NEEDBLOCK 0", out)
        self.assertIn("PANEL 1 0", out)
        self.assertTrue(any("full BAR" in l for l in out if l.startswith("NOTE")), out)

    def test_intel_gpu_with_a_framebuffer_driver_is_not_a_firmware_panel(self):
        out = main_flow(INSPIRON_119, config(-1, 0, igpu_props={"AAPL,ig-platform-id": b"\x00\x00\x52\x8a"}))
        self.assertIn("PANEL 0 0", out)
        self.assertEqual(edits(out), ["UEFI.Quirks.ResizeGpuBars|-integer|13", "Booter.Quirks.ResizeAppleGpuBars|-integer|-1"])
        self.assertIn("NEEDBLOCK 1", out)


NVDEV = ("nv_device_id() {", "DROP_ARGS=")


def nv_device(pci_text):
    """Setup's real NVIDIA device lookup and Turing check against a stub registry: 'NVDEV TURING WANT_ARGS'."""
    path = os.path.join(tempfile.mkdtemp(), "pci")
    pathlib.Path(path).write_text(pci_text)
    script = (f'ioreg() {{ cat "{path}"; }}\nWANT_ARGS=base\n' + cut(SETUP.read_text(), *NVDEV)
              + '\necho "$NVDEV $TURING $WANT_ARGS"\n')
    r = subprocess.run(["/bin/bash", "-c", script], capture_output=True, text=True, timeout=60)
    assert r.returncode == 0, r.stdout + r.stderr
    return r.stdout.strip()


class NvDeviceId(unittest.TestCase):
    def test_muxless_turing_3d_controller_is_found_and_gets_the_settle(self):
        # P14s Gen 2: Iris Xe + Quadro T500 (10DE:1FBB, class 0302)
        self.assertEqual(nv_device(pci(("IGPU@2", 0x8086, 0x9A49, VGA), ("GFX0@0", 0x10DE, 0x1FBB, D3D))),
                         "1fbb 1 base nvrmsettle=10000")

    def test_class_0380_turing_is_found(self):
        self.assertEqual(nv_device(pci(("IGPU@2", 0x8086, 0x9A68, VGA), ("GFX0@0", 0x10DE, 0x1F9D, OTHER))),
                         "1f9d 1 base nvrmsettle=10000")

    def test_a_display_card_wins_over_a_compute_card_listed_first(self):
        self.assertEqual(nv_device(pci(("GFX1@0", 0x10DE, 0x1FBB, D3D), ("GFX0@0", 0x10DE, 0x2D05, VGA),
                                       ("HDAU@0,1", 0x10DE, 0x22EB, AUDIO))), "2d05 0 base")

    def test_vga_turing_still_gets_the_settle(self):
        self.assertEqual(nv_device(pci(("GFX0@0", 0x10DE, 0x2184, VGA))), "2184 1 base nvrmsettle=10000")

    def test_no_nvidia_gpu(self):
        self.assertEqual(nv_device(pci(("IGPU@2", 0x8086, 0x9A49, VGA), ("HDEF@1f,3", 0x8086, 0xA0C8, AUDIO))), "0 base")


class FinishUpdate(unittest.TestCase):
    def test_intel_panel_laptop_keeps_the_installer_bars_and_iondrvsupport(self):
        self.assertEqual(finish(INSPIRON_119, config(**PREPARED)), (-1, 0, False))

    def test_a_lost_prepare_edit_is_put_back_for_the_panel(self):
        self.assertEqual(finish(INSPIRON_119, config(13, -1, block=True)), (-1, 0, False))

    def test_nvidia_only_desktop_gets_the_full_bar_and_the_exclude_back(self):
        self.assertEqual(finish(NVIDIA_DESKTOP, config(**PREPARED)), (13, -1, True))

    def test_desktop_with_igpu_on_gets_the_full_bar_back(self):
        self.assertEqual(finish(DESKTOP_IGPU_ON, config(**PREPARED)), (13, -1, False))

    def test_finish_leaves_what_setup_itself_writes(self):
        for name, machine in [("INSPIRON_119", INSPIRON_119), ("LAPTOP_HDMI_ON_NVIDIA", LAPTOP_HDMI_ON_NVIDIA),
                              ("HEADLESS_NVIDIA_NO_LID", HEADLESS_NVIDIA_NO_LID), ("NVIDIA_DESKTOP", NVIDIA_DESKTOP),
                              ("DESKTOP_IGPU_ON", DESKTOP_IGPU_ON), ("HEADLESS_NVIDIA_0380", HEADLESS_NVIDIA_0380),
                              ("DESKTOP_DISPLAY_AND_COMPUTE", DESKTOP_DISPLAY_AND_COMPUTE), ("DESKTOP_INTEL_0380", DESKTOP_INTEL_0380)]:
            with self.subTest(machine=name):
                self.assertEqual(finish(machine, config(**PREPARED)), after_setup(machine, config(**PREPARED)))


if __name__ == "__main__":
    unittest.main()
