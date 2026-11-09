// iasl -d of a ROG Zephyrus G16 DSDT (excerpt): EC0.KBLS is a read-only getter for the keyboard RGB state, reached from the WMI status call for device id 0x0010005B
            Method (KBLS, 0, NotSerialized)
            {
                Name (DUBF, Buffer (0x10)
                {
                     0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00   // ........
                })
                DUBF [Zero] = One
                DUBF [One] = KBLM /* \_SB_.PC00.LPCB.EC0_.KBLM */
                DUBF [0x02] = KBS1 /* \_SB_.PC00.LPCB.EC0_.KBS1 */
                DUBF [0x03] = KBS2 /* \_SB_.PC00.LPCB.EC0_.KBS2 */
                DUBF [0x04] = KBS3 /* \_SB_.PC00.LPCB.EC0_.KBS3 */
                DUBF [0x05] = KBSP /* \_SB_.PC00.LPCB.EC0_.KBSP */
                DUBF [0x06] = KBBS /* \_SB_.PC00.LPCB.EC0_.KBBS */
                DUBF [0x07] = Zero
                Return (DUBF) /* \_SB_.PC00.LPCB.EC0_.KBLS.DUBF */
            }
