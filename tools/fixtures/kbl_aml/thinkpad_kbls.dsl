// iasl -d of a ThinkPad E14 Gen 7 DSDT (user-uploaded firmware, excerpt): MLCG and MLCS are thin wrappers over the SMI helper KBLS
                Method (MLCG, 1, NotSerialized)
                {
                    Local0 = KBLS (Zero, Zero)
                    Return (Local0)
                }

                Method (MLCS, 1, NotSerialized)

                Method (MLCS, 1, NotSerialized)
                {
                    Local0 = KBLS (One, Arg0)
                    If (!(Local0 & 0x80000000))
                    {
                        If ((Arg0 & 0x00010000))
                        {
                            MHKQ (0x6001)
                        }
                        ElseIf (MHKK (One, 0x00020000))
                        {
                            MHKQ (0x1012)
                        }
                    }

                    Return (Local0)
                }

    Method (KBLS, 2, NotSerialized)
    {
        Return (SMI (0x14, 0x09, Arg0, Arg1, Zero))
    }
