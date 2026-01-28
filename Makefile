# macOS ARM64 build settings
export SDKROOT := /Library/Developer/CommandLineTools/SDKs/MacOSX15.sdk
export DEVELOPER_DIR := /Library/Developer/CommandLineTools

.PHONY: build release clean test check docker-build-ubuntu docker-release-ubuntu

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

# Docker build for Ubuntu (builds inside container, extracts binary)
docker-release-ubuntu:
	docker build --platform linux/amd64 -t whep-client-ubuntu -f Dockerfile.ubuntu .
	docker create --name whep-extract whep-client-ubuntu
	docker cp whep-extract:/workspace/target/release/whep-client ./whep-client-ubuntu
	docker rm whep-extract
	@echo "Built: ./whep-client-ubuntu"
