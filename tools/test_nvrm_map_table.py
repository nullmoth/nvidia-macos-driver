"""NVRM's kernel-mapping table holds what a resizable BAR1 needs, and its lookups stay as cheap as before.

Field logs from an RTX 3050 and an RTX 5070 showed 'high water 1024/1024' after about 20 minutes: every later
surface had no physical address and was refused. The table is now 8192 entries; every slot that is filled
raises the high-water mark (the alias path did not), and the lookups scan only up to it."""
import os, re, subprocess, unittest

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
SRC = "kexts/NVRM/os-xnu.cpp"


def problems(text):
    out = []
    m = re.search(r"#define NV_XNU_MAX_MAPS (\d+)", text)
    if not m or int(m.group(1)) < 8192:
        out.append("table smaller than 8192 entries")
    # every insert into the table must raise the high-water mark
    for ins in re.finditer(r"nv_xnu_maps\[i\] = \{ (?!NULL, NULL, NULL)[^}]*\};", text):
        after = text[ins.end():ins.end() + 200]
        if "nv_xnu_maps_high" not in after.split("return")[0]:
            out.append("an insert does not raise the high-water mark: " + ins.group(0)[:60])
    # the three lookups must stop at the high-water mark. Anchor on the DEFINITION, which starts a line
    # with its return type: searching for the bare name finds the first mention of it anywhere, and a
    # comment above the table that names these functions made this read the wrong body and report a
    # lookup as unbounded when it was not.
    for fn in ("nv_xnu_phys_for_kernel_va", "os_unmap_kernel_space", "nv_xnu_kmap_overlap"):
        d = re.search(r"^[A-Za-z_][^\n/]*\b%s\s*\(" % re.escape(fn), text, re.M)
        if not d:
            out.append(fn + " has no definition in this file")
            continue
        body = text[d.start():]
        body = body[:body.find("\n}\n")]
        if "i < nv_xnu_maps_high" not in body:
            out.append(fn + " scans the whole table")
    return out


class MapTable(unittest.TestCase):
    def test_table_is_large_and_lookups_are_bounded(self):
        with open(os.path.join(ROOT, SRC)) as f:
            self.assertEqual(problems(f.read()), [])

    def test_negative_control_the_previous_source_fails(self):
        old = subprocess.run(["git", "-C", ROOT, "show", "042bebe:" + SRC], capture_output=True, text=True, check=True).stdout
        found = problems(old)
        self.assertIn("table smaller than 8192 entries", found)
        self.assertTrue(any("does not raise" in p for p in found), found)


if __name__ == "__main__":
    unittest.main()
