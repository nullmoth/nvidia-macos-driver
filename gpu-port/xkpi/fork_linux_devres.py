# fork_linux_devres.py <pristine linux_devres.c> <out>: LinuxKPI's devres code plus Linux's devm_release_action
# (run one registered devm action now and drop it, so it does not run again at detach). It is appended inside the
# same file because the action records and their release function are private to it. Anchor asserted; atomic write.
import os, sys
s = open(sys.argv[1]).read()
anchor = "\nlkpi_devm_add_action_or_reset(struct device *dev, void (*action)(void *), void *data)\n"
assert s.count(anchor) == 1
s += r"""
/* macOS port: Linux's devm_release_action() */
static int
lkpi_devm_action_match(struct device *dev __unused, void *res, void *mp)
{
	struct devres_action *a = res, *m = mp;

	return (a->action == m->action && a->data == m->data);
}

void
lkpi_devm_release_action(struct device *dev, void (*action)(void *), void *data)
{
	struct devres_action m = { .data = data, .action = action };
	struct devres *dr;

	spin_lock(&dev->devres_lock);
	dr = lkpi_devres_find_dr(dev, lkpi_devm_action_release, lkpi_devm_action_match, &m);
	if (dr != NULL)
		lkpi_devres_unlink_locked(dev, dr);
	spin_unlock(&dev->devres_lock);
	if (dr == NULL) {
		dev_warn(dev, "%s: no such devm action\n", __func__);
		return;
	}
	action(data);
	lkpi_devres_free_dr(dr);
}
"""
open(sys.argv[2] + ".tmp", "w").write(s)
os.replace(sys.argv[2] + ".tmp", sys.argv[2])
print("linux_devres.c fork applied")
