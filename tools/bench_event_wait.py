#!/usr/bin/env python3
"""Compare NVRM_EVENT_WAIT in disposable processes; never edit driver settings."""

import argparse
import json
import math
import os
from pathlib import Path
import statistics
import subprocess
import tempfile


def summarize(runs):
    result = {}
    for mode in ("compute", "image"):
        groups = {}
        for policy in ("poll", "event"):
            selected = [r for r in runs if r["mode"] == mode and r["policy"] == policy]
            if not selected:
                raise ValueError("missing " + mode + "/" + policy + " samples")
            for run in selected:
                for field in ("samples_ms", "samples_cpu_ms"):
                    values = run[field]
                    if len(values) != 25 or not all(math.isfinite(v) and v >= 0 for v in values):
                        raise ValueError("invalid measurement samples")
            groups[policy] = {
                "runs": len(selected),
                "samples": sum(len(r["samples_ms"]) for r in selected),
                "median_wall_ms": statistics.median(statistics.median(r["samples_ms"]) for r in selected),
                "median_cpu_ms": statistics.median(statistics.median(r["samples_cpu_ms"]) for r in selected),
            }
        result[mode] = groups
    return result


def windowserver():
    return subprocess.check_output(["pgrep", "-x", "WindowServer"], text=True).strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path, help="new directory for samples and local logs")
    parser.add_argument("--rounds", type=int, default=5)
    args = parser.parse_args()
    if args.rounds < 1:
        parser.error("--rounds must be positive")
    args.output.mkdir(parents=True, exist_ok=False)
    environment = dict(os.environ)
    for key in list(environment):
        if key.startswith(("DYLD_", "GPU_PROBE_")):
            environment.pop(key)
    runs = []
    image_hash = None
    initial_windowserver = windowserver()

    with tempfile.TemporaryDirectory(prefix="nvmtl-eventwait-") as temporary:
        binary = str(Path(temporary) / "probe")
        subprocess.run([
            "xcrun", "clang", "-O2", "-fobjc-arc",
            str(Path(__file__).with_suffix(".m")),
            "-framework", "Foundation", "-framework", "Metal",
            "-framework", "CoreGraphics", "-framework", "CoreImage", "-o", binary,
        ], check=True, env=environment)

        def probe(label, mode, policy=None):
            nonlocal image_hash
            if windowserver() != initial_windowserver:
                raise RuntimeError("WindowServer restarted; stop before more GPU work")
            command = [binary, mode]
            if policy is not None:
                command.append("NVRM_EVENT_WAIT=" + ("1" if policy == "event" else "0"))
            try:
                proc = subprocess.run(command, capture_output=True, text=True, timeout=55, env=environment)
            except subprocess.TimeoutExpired as error:
                (args.output / (label + ".timeout.txt")).write_text(str(error))
                raise RuntimeError("probe timed out; stop before more GPU work") from error
            (args.output / (label + ".stderr.txt")).write_text(proc.stderr)
            if proc.returncode or "implemented in both" in proc.stderr or windowserver() != initial_windowserver:
                raise RuntimeError("probe failed or WindowServer restarted; inspect " + label)
            data = json.loads(proc.stdout)
            if mode in ("compute", "image"):
                enabled = "event wait ON" in proc.stderr
                if enabled != (policy == "event") or "event wait OFF" in proc.stderr:
                    raise RuntimeError("requested wait policy was not verified: " + label)
            if mode in ("compute", "events") and data.get("verified") is not True:
                raise RuntimeError("data verification failed: " + label)
            if mode == "transfer" and not all(t["verified"] for t in data["tests"]):
                raise RuntimeError("transfer verification failed: " + label)
            if mode == "image":
                if not data["alpha_verified"]:
                    raise RuntimeError("image alpha verification failed")
                if image_hash is not None and data["pixel_hash"] != image_hash:
                    raise RuntimeError("image pixels changed between runs")
                image_hash = data["pixel_hash"]
            data.update(policy=policy, windowserver_unchanged=True)
            (args.output / (label + ".json")).write_text(json.dumps(data, indent=2) + "\n")
            print(label + ": passed", flush=True)
            return data

        probe("inventory", "inventory")
        for round_number in range(1, args.rounds + 1):
            policies = ("poll", "event") if round_number % 2 else ("event", "poll")
            for mode in ("compute", "image"):
                for policy in policies:
                    data = probe("{}-{}-{}".format(mode, policy, round_number), mode, policy)
                    data["round"] = round_number
                    runs.append(data)
        for mode in ("events", "transfer"):
            for policy in ("poll", "event"):
                probe(mode + "-" + policy, mode, policy)
        result = {"summary": summarize(runs), "runs": runs}
        (args.output / "comparison.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(result["summary"], indent=2))


if __name__ == "__main__":
    main()
