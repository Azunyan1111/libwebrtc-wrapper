# libwebrtc-wrapper Makefile

# WebRTC build version
WEBRTC_VERSION := m144.7559.2.2
WEBRTC_BASE_URL := https://github.com/shiguredo-webrtc-build/webrtc-build/releases/download/$(WEBRTC_VERSION)

# Detect OS and Architecture
UNAME_S := $(shell uname -s)
UNAME_M := $(shell uname -m)

# Set target based on OS and architecture
ifeq ($(UNAME_S),Darwin)
    ifeq ($(UNAME_M),arm64)
        WEBRTC_TARGET := macos_arm64
    else ifeq ($(UNAME_M),x86_64)
        WEBRTC_TARGET := macos_x86_64
    endif
else ifeq ($(UNAME_S),Linux)
    ifeq ($(UNAME_M),x86_64)
        WEBRTC_TARGET := ubuntu-24.04_x86_64
    else ifeq ($(UNAME_M),aarch64)
        WEBRTC_TARGET := ubuntu-24.04_arm64
    endif
endif

# Download URL and file names
WEBRTC_TARBALL := webrtc.$(WEBRTC_TARGET).tar.gz
WEBRTC_URL := $(WEBRTC_BASE_URL)/$(WEBRTC_TARBALL)

# Output directory structure: deps/webrtc/$(WEBRTC_TARGET)/
DEPS_DIR := deps
WEBRTC_DIR := $(DEPS_DIR)/webrtc/$(WEBRTC_TARGET)

.PHONY: download clean info

info:
	@echo "OS: $(UNAME_S)"
	@echo "Architecture: $(UNAME_M)"
	@echo "Target: $(WEBRTC_TARGET)"
	@echo "Download URL: $(WEBRTC_URL)"
	@echo "Output Directory: $(WEBRTC_DIR)"

download:
	@echo "Downloading libwebrtc for $(WEBRTC_TARGET)..."
	@if [ -z "$(WEBRTC_TARGET)" ]; then \
		echo "Error: Unsupported OS/Architecture combination: $(UNAME_S)/$(UNAME_M)"; \
		exit 1; \
	fi
	@mkdir -p $(WEBRTC_DIR)
	@curl -L -o $(WEBRTC_TARBALL) $(WEBRTC_URL)
	@echo "Extracting $(WEBRTC_TARBALL) to $(WEBRTC_DIR)..."
	@tar -xzf $(WEBRTC_TARBALL) -C $(WEBRTC_DIR) --strip-components=1
	@rm -f $(WEBRTC_TARBALL)
	@echo "Done. libwebrtc extracted to $(WEBRTC_DIR)/"

clean:
	@echo "Cleaning up..."
	@rm -rf $(DEPS_DIR)
	@rm -f webrtc.*.tar.gz
	@echo "Done."
