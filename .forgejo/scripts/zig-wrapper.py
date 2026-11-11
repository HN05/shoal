#!/usr/bin/env python3
"""Adapt rustc's linker arguments to the Zig version in the CI image."""

import os
import sys


args = sys.argv[1:]
if args and args[0] in ("cc", "c++") and (sdk := os.environ.get("SDKROOT")):
    # Zig prefixes even absolute Cargo library paths with --sysroot and warns
    # that the bundled SDK has no Frameworks directory. Let Zig find its
    # bundled SDK itself; rustc still sees SDKROOT.
    args = [arg for arg in args if arg not in (
        f"--sysroot={sdk}", f"-L{sdk}/usr/lib", f"-F{sdk}/System/Library/Frameworks",
    )]
zig = os.environ["CI_ZIG_BINARY"]
os.execv(zig, [zig, *args])
