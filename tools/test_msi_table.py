#!/usr/bin/env python3
"""gen_msi_table.py reads the msi-ec firmware table. A sample in the driver's own layout is parsed, and when
NULLMOTH_TEST_MSI_EC points at a copy of the driver's msi-ec.c the committed header is checked against it."""
import importlib.util
import os
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
KDIR = os.path.join(HERE, '..', 'kexts', 'NMKbdBacklight')
spec = importlib.util.spec_from_file_location('gen', os.path.join(KDIR, 'gen_msi_table.py'))
gen = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gen)

SAMPLE = '''
static const char * const ALLOWED_FW_0[] __initconst = {
	"14C1EMS1.012",
	"14C1EMS1.101",
	NULL
};

static struct msi_ec_conf CONF0 __initdata = {
	.allowed_fw = ALLOWED_FW_0,
	.kbd_bl = {
		.bl_mode_address  = 0x2c, // ?
		.bl_state_address = 0xf3,
		.state_base_value = 0x80,
		.max_state        = 3,
	},
};

static const char * const ALLOWED_FW_1[] __initconst = {
	"1552EMS1.118",
	NULL
};

static struct msi_ec_conf CONF1 __initdata = {
	.allowed_fw = ALLOWED_FW_1,
	.kbd_bl = {
		.bl_mode_address  = 0x2c, // ?
		.bl_state_address = MSI_EC_ADDR_UNSUPP, // 0xd3, not functional
		.state_base_value = 0x80,
		.max_state        = 3,
	},
};
'''


class Table(unittest.TestCase):
    def test_a_usable_configuration_gives_one_row_per_firmware_string(self):
        self.assertEqual(gen.parse(SAMPLE), [('14C1EMS1.012', 0xf3, 0x80, 3), ('14C1EMS1.101', 0xf3, 0x80, 3)])

    def test_a_configuration_marked_not_functional_is_left_out_so_its_firmware_is_refused(self):
        self.assertNotIn('1552EMS1.118', [r[0] for r in gen.parse(SAMPLE)])

    def test_the_header_lists_what_was_parsed(self):
        out = gen.render(gen.parse(SAMPLE))
        self.assertIn('{ "14C1EMS1.012", 0xf3, 0x80, 3 }', out)

    @unittest.skipUnless(os.environ.get('NULLMOTH_TEST_MSI_EC'), 'set NULLMOTH_TEST_MSI_EC to a copy of msi-ec.c')
    def test_the_committed_header_is_what_the_driver_table_gives(self):
        with open(os.environ['NULLMOTH_TEST_MSI_EC']) as f:
            want = gen.render(gen.parse(f.read()))
        with open(os.path.join(KDIR, 'msi_table.h')) as f:
            self.assertEqual(f.read(), want)


if __name__ == '__main__':
    unittest.main()
