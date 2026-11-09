"""Instruction words for the device-side __assertfail stand-in, produced from NAK's own encoder source.

The stand-in is what native DLSS kernels reach when an assert fires: a faulting store followed by EXIT. The encodings come
from NAK (Mesa, MIT): src/nouveau/compiler/nak/sm70_encode.rs, `impl SM70Op for OpExit` and `impl SM70Op for OpSt`
(global space, UGPR form), plus SM70Encoder::set_instr_deps for the scheduling bits. nak_words() reads that file from the
Mesa tree being patched and checks every constant it relies on against the source text, so a Mesa change that moves one of
them stops the build instead of producing a wrong word. The same module is run by legal/stub_selftest.py."""
import re


def _src(tree):
    return open(tree + "/src/nouveau/compiler/nak/sm70_encode.rs").read()


def _impl(s, name):
    a = s.index("impl SM70Op for %s {" % name)
    b = s.index("\nimpl SM70Op for ", a + 10)
    return s[a:b]


def _need(body, text, what):
    assert text in body, "NAK encoder changed (%s): %r not found" % (what, text)


def nak_words(tree, sm):
    """Return the four 64-bit words of [ST.E.32 [RZ+UR63], RZ ; EXIT] pairs: (st_lo, st_hi, exit_lo, exit_hi)."""
    s = _src(tree)
    ex, st = _impl(s, "OpExit"), _impl(s, "OpSt")
    _need(ex, "e.set_opcode(0x94d);", "EXIT opcode")
    _need(ex, "e.set_field(87..90, 0x7_u8);", "EXIT predicate")
    _need(ex, "e.set_field(90..91, false);", "EXIT predicate not")
    _need(st, "e.set_opcode(0x986);", "ST global UGPR opcode")
    _need(st, "e.set_reg_addr(24..32, &self.addr, 90);", "ST GPR address")
    _need(st, "e.set_ureg_addr(64, &self.uniform_addr, 72);", "ST UGPR address")
    _need(st, "e.set_reg_src(32..40, &self.data);", "ST data")
    _need(st, "e.set_field(40..64, self.offset);", "ST offset")
    _need(st, "e.set_bit(91, has_ugpr);", "ST UGPR flag")
    _need(s, "self.set_field(105..109, deps.delay);", "delay")
    _need(s, "self.set_field(110..113, deps.wr_bar().unwrap_or(7));", "write barrier")
    _need(s, "self.set_field(113..116, deps.rd_bar().unwrap_or(7));", "read barrier")
    _need(s, "self.set_field(116..122, deps.wt_bar_mask);", "wait mask")
    _need(s, "self.set_bit(109, deps.yld);", "yield")
    _need(s, "fn set_mem_type", "mem type")
    # register encodings used above: GPR zero = 255, true predicate = 7, UGPR zero = 63 before SM100
    _need(s, "RegFile::GPR => 255,", "GPR zero")
    _need(s, "RegRef::new(file, 7, 1)", "true predicate")
    _need(s, "            63\n", "UGPR max before SM100")
    assert 73 <= sm < 100, "stub is defined for SM75..SM99"

    def put(w, lo, n, v):
        assert 0 <= v < (1 << n)
        return w | (v << lo)

    def sched(w):
        w = put(w, 105, 4, 5)       # delay 5 cycles
        w = put(w, 109, 1, 1)       # yield
        w = put(w, 110, 3, 7)       # no write barrier
        w = put(w, 113, 3, 7)       # no read barrier
        return w                    # wait mask 0, reuse 0

    e = 0
    e = put(e, 0, 12, 0x94d)
    e = put(e, 12, 3, 7)            # @PT
    e = put(e, 87, 3, 7)            # predicate (PT)
    e = sched(e)
    t = 0
    t = put(t, 0, 12, 0x986)
    t = put(t, 12, 3, 7)            # @PT
    t = put(t, 24, 8, 255)          # address: RZ (32-bit zero)
    t = put(t, 90, 1, 0)
    t = put(t, 64, 6, 63)           # uniform address: URZ (64-bit zero) => address 0
    t = put(t, 72, 1, 1)
    t = put(t, 73, 3, 4)            # 32-bit
    if sm < 80:
        t = put(t, 79, 2, 1)        # weak ordering, CTA scope (pre-SM80 encoding)
    t = put(t, 84, 3, 1)            # normal eviction priority
    t = put(t, 32, 8, 255)          # data: RZ
    t = put(t, 40, 24, 0)           # offset 0
    t = put(t, 91, 1, 1)
    t = sched(t)
    m = (1 << 64) - 1
    return (t & m, t >> 64, e & m, e >> 64)


def c_array(tree, sm):
    st_lo, st_hi, ex_lo, ex_hi = nak_words(tree, sm)
    return ("0x%016xull, 0x%016xull,   /* ST.E.32 [RZ+URZ], RZ: store to address 0, faults */\n"
            "         0x%016xull, 0x%016xull,   /* EXIT */\n"
            "         0x%016xull, 0x%016xull,   /* EXIT (never reached) */\n"
            "         0x%016xull, 0x%016xull,") % (st_lo, st_hi, ex_lo, ex_hi, ex_lo, ex_hi, ex_lo, ex_hi)
