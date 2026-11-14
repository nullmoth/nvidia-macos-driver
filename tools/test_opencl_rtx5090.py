#!/usr/bin/env python3
"""Include the process-local OpenCL launcher contracts in the host test suite."""
from pathlib import Path
import subprocess
import sys


if __name__ == "__main__":
    subprocess.run(
        [sys.executable, "-B", str(Path(__file__).with_name("opencl-rtx5090") /
                                  "launcher" / "test_launcher.py")],
        check=True,
    )
