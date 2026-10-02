#!/usr/bin/env python3
"""Compatibility entry point for the provider-neutral path hook."""

from boundary_path_hook import main


if __name__ == "__main__":
    raise SystemExit(main())
