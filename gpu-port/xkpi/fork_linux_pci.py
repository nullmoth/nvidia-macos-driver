# fork_linux_pci.py <override/src/linux_pci.c>: macOS forms of the FreeBSD-specific parts of LinuxKPI's linux_pci.c.
# The PCI attach, BAR, MSI, DMA and DMA-pool code stays LinuxKPI's; it runs on the port's newbus and bus_dma.
import pathlib, sys
p = pathlib.Path(sys.argv[1]); s = p.read_text()

def replace_once(old, new):
    global s
    assert s.count(old) == 1, (old[:70], s.count(old))
    s = s.replace(old, new)

# FreeBSD reaches UMA through headers this build does not include (sys/mbuf.h via the network stack)
replace_once("#include <vm/vm.h>\n", "#include <vm/vm.h>\n#include <vm/uma.h>\n")

p.write_text(s)
print("linux_pci.c fork applied")
