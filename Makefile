# macOS ARM64 build settings
export SDKROOT := /Library/Developer/CommandLineTools/SDKs/MacOSX15.sdk
export DEVELOPER_DIR := /Library/Developer/CommandLineTools

# WebRTC release version
WEBRTC_VERSION := webrtc-0001d84-2
WEBRTC_BASE_URL := https://github.com/livekit/rust-sdks/releases/download/$(WEBRTC_VERSION)

# Download destinations
DEPS_DIR := deps/webrtc
MAC_ARM64_DIR := $(DEPS_DIR)/mac_arm64
LINUX_X64_DIR := $(DEPS_DIR)/linux_x64

.PHONY: build release clean test check docker-build-ubuntu docker-release-ubuntu download download-mac-arm64 download-linux-x64

# Download all WebRTC libraries
download: download-mac-arm64 download-linux-x64
	@echo "All WebRTC libraries downloaded successfully"

# Download macOS ARM64 WebRTC library
download-mac-arm64:
	@echo "Downloading WebRTC for macOS ARM64..."
	@mkdir -p $(MAC_ARM64_DIR)
	@curl -L -o /tmp/webrtc-mac-arm64.zip $(WEBRTC_BASE_URL)/webrtc-mac-arm64-release.zip
	@unzip -o /tmp/webrtc-mac-arm64.zip -d /tmp/
	@rm -rf $(MAC_ARM64_DIR)/*
	@mv /tmp/mac-arm64-release/* $(MAC_ARM64_DIR)/
	@rm -rf /tmp/mac-arm64-release /tmp/webrtc-mac-arm64.zip
	@echo "macOS ARM64 WebRTC extracted to $(MAC_ARM64_DIR)"

# Download Linux x64 WebRTC library
download-linux-x64:
	@echo "Downloading WebRTC for Linux x64..."
	@mkdir -p $(LINUX_X64_DIR)
	@curl -L -o /tmp/webrtc-linux-x64.zip $(WEBRTC_BASE_URL)/webrtc-linux-x64-release.zip
	@unzip -o /tmp/webrtc-linux-x64.zip -d /tmp/
	@rm -rf $(LINUX_X64_DIR)/*
	@mv /tmp/linux-x64-release/* $(LINUX_X64_DIR)/
	@rm -rf /tmp/linux-x64-release /tmp/webrtc-linux-x64.zip
	@echo "Linux x64 WebRTC extracted to $(LINUX_X64_DIR)"

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
