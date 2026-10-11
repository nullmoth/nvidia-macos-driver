# fork_headers.py <freebsd-src> <override/include>: macOS forms of LinuxKPI headers, made from the pristine copies.
import pathlib, sys
src = pathlib.Path(sys.argv[1]) / "sys/compat/linuxkpi/common/include"
dst = pathlib.Path(sys.argv[2])

def fork(rel, pairs):
    s = (src / rel).read_text()
    for old, new in pairs:
        assert s.count(old) == 1, (rel, old[:60], s.count(old))
        s = s.replace(old, new)
    out = dst / rel
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(s)
    print("forked", rel)

# jiffies: FreeBSD's is a variable the clock interrupt advances; the port reads the clock when asked, so code that
# spins on jiffies sees it move without any timer having to run
fork("linux/sort.h", [
    ("\tqsort_r(base, num, size, cmp, priv);\t\t\\\n",
     "\tqsort_r(base, num, size, (int (*)(const void *, const void *, void *))(cmp), priv); \\\n"),
])

fork("linux/jiffies.h", [
    ("extern unsigned long jiffies;\t/* defined in sys/kern/subr_ticks.S */\n",
     "unsigned long nmx_jiffies(void);\t/* macOS port: hz ticks since load, read from the clock (nmx_kernel.c) */\n"
     "#define\tjiffies\t\tnmx_jiffies()\n"),
])
