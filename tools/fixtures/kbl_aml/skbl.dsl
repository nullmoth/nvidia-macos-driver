// iasl -d of a HONOR MagicBook DSDT (excerpt): SKBL does nothing but answer STAT=1
    Method (SKBL, 1, NotSerialized)
    {
        Name (BUFF, Buffer (0x0100){})
        CreateByteField (BUFF, Zero, STAT)
        STAT = One
        Return (BUFF) /* \SKBL.BUFF */
    }
// iasl -d of a Lenovo Yoga 14sARE DSDT (excerpt): SKBL is an SMI helper under HALS/SALS
        Method (SKBL, 1, NotSerialized)
        {
            QSMI (0x14, 0x04, Arg0, Zero, Zero)
        }
