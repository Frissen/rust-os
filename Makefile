# Minimal Makefile around `cargo bootimage` + QEMU.
# Run `make setup` once, then `make run` to boot the kernel.

KERNEL_BIN := target/x86_64-rust_os/debug/bootimage-rust_os.bin

QEMU_ARGS := \
	-drive format=raw,file=$(KERNEL_BIN) \
	-serial stdio \
	-no-reboot \
	-netdev user,id=u1 \
	-device rtl8139,netdev=u1 \
	-object filter-dump,id=f1,netdev=u1,file=/tmp/qemu-net.pcap

.PHONY: setup build run run-headless test clean

setup:
	./scripts/setup.sh

build:
	cargo bootimage

run: build
	qemu-system-x86_64 $(QEMU_ARGS)

# Headless run — useful for CI / smoke-testing. Caller is expected to bound
# runtime themselves (e.g. `timeout 5 make run-headless`) since the kernel
# loops forever waiting for keypresses.
run-headless: build
	qemu-system-x86_64 -display none $(QEMU_ARGS)

test:
	cargo test

clean:
	cargo clean
