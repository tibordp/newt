VERSION := $(shell cargo metadata --format-version 1 --no-deps | python3 -c "import sys,json; [print(p['version']) for p in json.load(sys.stdin)['packages'] if p['name']=='newt-agent']")

DIST := agents

LINUX_TARGETS := x86_64-unknown-linux-musl aarch64-unknown-linux-musl
DARWIN_TARGETS := aarch64-apple-darwin x86_64-apple-darwin

# sha256sum on Linux, shasum -a 256 on macOS
SHA256 := $(shell command -v sha256sum 2>/dev/null || echo "shasum -a 256")

# ─── Host detection ───
#
# Linux-musl targets go through `cargo zigbuild`, which uses zig as the
# cross-linker and works natively on any host. Darwin targets use plain
# `cargo build` — rustc can produce either darwin arch from either darwin
# host without extra toolchain. Cross-compiling to darwin from Linux needs
# osxcross + Apple's SDK (license-restricted to Apple hardware), so we
# skip darwin on Linux hosts.

HOST_OS := $(shell uname -s)
HOST_ARCH := $(shell uname -m)
ifeq ($(HOST_ARCH),arm64)
  HOST_ARCH := aarch64
endif

ifeq ($(HOST_OS),Darwin)
  BUILDABLE := $(DARWIN_TARGETS) $(LINUX_TARGETS)
  UNBUILDABLE :=
else ifeq ($(HOST_OS),Linux)
  BUILDABLE := $(LINUX_TARGETS)
  UNBUILDABLE := $(DARWIN_TARGETS)
else
  $(error Unsupported host OS: $(HOST_OS))
endif

# Per-triple cache dir keeps each target's artifacts isolated from every
# other target and from the main dev build in ./target.
CARGO_TARGET_DIR_FOR = target-agents/$(1)

.PHONY: agents linux-agents darwin-agents clean install help

help:
	@echo "Host:   $(HOST_OS)/$(HOST_ARCH)"
	@echo "Build:  $(BUILDABLE)"
	@if [ -n "$(UNBUILDABLE)" ]; then echo "Skip:   $(UNBUILDABLE) (cannot cross-compile from this host)"; fi
	@echo ""
	@echo "Linux agents:  'cargo zigbuild' (requires: cargo-zigbuild + zig)"
	@echo "Darwin agents: 'cargo build'"
	@echo ""
	@echo "Targets:"
	@echo "  make agents         build every agent this host can produce"
	@echo "  make linux-agents   linux agents only"
	@echo "  make darwin-agents  darwin agents only"
	@echo "  make clean          remove agents/ and target-agents/"

agents: $(addprefix $(DIST)/,$(addsuffix /newt-agent,$(BUILDABLE)))

linux-agents: $(addprefix $(DIST)/,$(addsuffix /newt-agent,$(filter %-unknown-linux-musl,$(BUILDABLE))))

darwin-agents: $(addprefix $(DIST)/,$(addsuffix /newt-agent,$(filter %-apple-darwin,$(BUILDABLE))))

# Linux-musl targets use cargo-zigbuild (zig as cross-linker).
define linux_rule
$(DIST)/$(1)/newt-agent: FORCE
	CARGO_TARGET_DIR=$(call CARGO_TARGET_DIR_FOR,$(1)) cargo zigbuild --release --target $(1) -p newt-agent
	@mkdir -p $$(dir $$@)
	cp $(call CARGO_TARGET_DIR_FOR,$(1))/$(1)/release/newt-agent $$@
	$(SHA256) $$@ > $$@.sha256
endef

# Darwin targets: native cargo build. Both arches can be produced from
# either darwin host without additional toolchain setup.
define darwin_rule
$(DIST)/$(1)/newt-agent: FORCE
	CARGO_TARGET_DIR=$(call CARGO_TARGET_DIR_FOR,$(1)) cargo build --release --target $(1) -p newt-agent
	@mkdir -p $$(dir $$@)
	cp $(call CARGO_TARGET_DIR_FOR,$(1))/$(1)/release/newt-agent $$@
	$(SHA256) $$@ > $$@.sha256
endef

$(foreach t,$(filter %-unknown-linux-musl,$(BUILDABLE)),$(eval $(call linux_rule,$(t))))
$(foreach t,$(filter %-apple-darwin,$(BUILDABLE)),$(eval $(call darwin_rule,$(t))))

clean:
	rm -rf $(DIST) target-agents

# ─── Install target (used by packaging scripts) ───

PREFIX ?= /usr
DESTDIR ?=
AGENT_DIR ?= agents
BINARY ?= target/release/newt

install:
	install -Dm755 $(BINARY) $(DESTDIR)$(PREFIX)/bin/newt
	for triple_dir in $(AGENT_DIR)/*/; do \
		triple=$$(basename "$$triple_dir"); \
		install -Dm755 "$$triple_dir/newt-agent" \
			"$(DESTDIR)$(PREFIX)/share/newt/agents/$$triple/newt-agent"; \
	done
	install -Dm644 packaging/newt.desktop $(DESTDIR)$(PREFIX)/share/applications/newt.desktop
	install -Dm644 src-tauri/icons/32x32.png $(DESTDIR)$(PREFIX)/share/icons/hicolor/32x32/apps/newt.png
	install -Dm644 src-tauri/icons/128x128.png $(DESTDIR)$(PREFIX)/share/icons/hicolor/128x128/apps/newt.png
	install -Dm644 src-tauri/icons/128x128@2x.png $(DESTDIR)$(PREFIX)/share/icons/hicolor/256x256/apps/newt.png

FORCE:
