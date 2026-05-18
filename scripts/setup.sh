#!/usr/bin/env bash
# scripts/setup.sh — one-shot setup for building this kernel.
#
# Modern Rust nightlies have drifted away from the assumptions baked into
# `bootloader = "0.9"` and `bootimage = "0.10"`. Rather than chase an exact
# historical toolchain, we install both and apply two surgical patches:
#   1. bootimage passes `-Zjson-target-spec` to cargo, but that flag was
#      removed from cargo in mid-2023. We delete the line.
#   2. The bootloader's bundled `x86_64-bootloader.json` uses unquoted ints for
#      `target-pointer-width` / `target-c-int-width`, which newer rustc rejects.
#      We quote them. We also drop its v4 Cargo.lock so older cargos can parse
#      it.
#
# Re-running this script is safe: each patch is idempotent.

set -euo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_DIR"

echo "==> Installing rust nightly toolchain (pinned by rust-toolchain.toml)"
rustup show active-toolchain >/dev/null

echo "==> Installing bootimage from upstream git"
cargo install --git https://github.com/rust-osdev/bootimage bootimage --force

BOOTIMAGE_SRC="$(find "$HOME/.cargo/git/checkouts/bootimage-"* -maxdepth 2 -name bootloader.rs 2>/dev/null | head -n1)"
if [[ -n "$BOOTIMAGE_SRC" ]]; then
    echo "==> Patching bootimage: drop removed cargo flag (-Zjson-target-spec)"
    sed -i '/cmd.arg("-Zjson-target-spec")/d' "$BOOTIMAGE_SRC"
    BOOTIMAGE_CRATE="$(dirname "$(dirname "$BOOTIMAGE_SRC")")"
    BOOTIMAGE_CRATE="$(dirname "$BOOTIMAGE_CRATE")"
    (cd "$BOOTIMAGE_CRATE" && cargo install --path . --force)
fi

# We need the bootloader source cached locally before we can patch it. A
# throw-away `cargo fetch` populates the registry cache.
echo "==> Fetching dependencies"
cargo fetch

BOOTLOADER_CRATE="$(ls -d "$HOME/.cargo/registry/src/"index.crates.io-*/"bootloader-0.9."* 2>/dev/null | head -n1)"
if [[ -z "$BOOTLOADER_CRATE" ]]; then
    echo "ERROR: could not locate the bootloader 0.9.x crate in cargo registry"
    exit 1
fi
echo "==> Patching $BOOTLOADER_CRATE"
# Drop incompatible v4 lockfile.
rm -f "$BOOTLOADER_CRATE/Cargo.lock"
# Quote pointer-width and c-int-width in the bootloader's target spec.
python3 - "$BOOTLOADER_CRATE/x86_64-bootloader.json" <<'PY'
import json, sys
path = sys.argv[1]
with open(path) as f:
    spec = json.load(f)
if isinstance(spec.get("target-pointer-width"), int):
    spec["target-pointer-width"] = str(spec["target-pointer-width"])
if isinstance(spec.get("target-c-int-width"), int):
    spec["target-c-int-width"] = str(spec["target-c-int-width"])
spec.pop("rustc-abi", None)
with open(path, "w") as f:
    json.dump(spec, f, indent=4)
PY

echo "==> Setup complete. Run 'make run' to build and boot the kernel in QEMU."
