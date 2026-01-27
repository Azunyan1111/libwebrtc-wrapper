# macOS ARM64 build settings
export SDKROOT := /Library/Developer/CommandLineTools/SDKs/MacOSX15.sdk
export DEVELOPER_DIR := /Library/Developer/CommandLineTools

.PHONY: build release clean test check

build:
	cargo build

release:
	cargo build --release

clean:
	cargo clean

test:
	cargo test

check:
	cargo check
