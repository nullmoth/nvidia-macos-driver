# Contributing

Bug reports and pull requests are welcome. Please read this first; it keeps the queue small enough that every PR
gets a real review.

## Reporting a bug

1. Press Send logs in the 1401 app (Windows or Mac). It gives you a report ID like `NM-ABCD1234`.
2. Open an issue with the bug form and put the report ID in it.
3. Describe what you see in a few sentences of your own. The logs are in the report, so don't paste them in.

One problem per issue. If you found the cause, say how you know: what you ran and what it printed.

## Pull requests

- Write it yourself and understand it. You may use whatever tools you like, but you have to be able to explain
  every line and answer review questions about it. PRs that are pasted model output (long generated descriptions,
  "say the word and I will add it", changes the author can't explain) will be closed without review.
- One change per PR. A fix, a feature or a cleanup, not all three. Small PRs get merged; 2,000-line PRs sit.
- Say what you tested. Your card, macOS version, and what you did to check it. "Builds" is not tested.
- Keep the description short: what was wrong, what you changed, how you tested it. Link the issue.
- Commit messages: `component: short summary` under 72 characters (`NVRMFB: fix 165 Hz mode missing`), then a
  plain explanation if one is needed.
- Comments in code explain why the code does what it does. They are not a log of when you found something.

Kernel changes (`kexts/`) need a boot on real hardware before they can be merged, because a bad kext stops the
machine from starting. Say which card you booted it on.

## Where help is needed

See the issues labelled `help wanted`. The areas with the most open problems:

- Turing (GTX 16, RTX 20): flicker and black screens on cards with a small BAR.
- Display: ultrawide and high refresh rate modes, hot-plugging a monitor.
- Video: hardware decode in sandboxed processes (Chrome, Photos).
- Laptops: machines where the screen is wired to the Intel GPU.

If you own a card we haven't tested, a working report (the "It works" form) is as useful as code.

## Building

See [docs/HOW-IT-WORKS.md](docs/HOW-IT-WORKS.md) for the layout of the source and how each part is built.
