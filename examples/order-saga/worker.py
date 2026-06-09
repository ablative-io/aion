#!/usr/bin/env python3
"""Compatibility entry point for the order-saga worker.

The documented worker lives at `examples/order-saga/worker/worker.py`. This
wrapper keeps the older `examples/order-saga/worker.py` path usable.
"""

from __future__ import annotations

from pathlib import Path
import runpy

if __name__ == "__main__":
    worker_path = Path(__file__).with_name("worker") / "worker.py"
    runpy.run_path(str(worker_path), run_name="__main__")
