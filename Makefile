# `cargo xtask agents` is how agent binaries are built and laid out — on
# Linux, macOS, Windows and CI alike. The one cross-platform
# implementation lives in the `xtask` crate; these Make targets just
# forward to it.

.PHONY: agents linux-agents darwin-agents windows-agents clean install help

help:
	cargo xtask help

agents:
	cargo xtask agents

linux-agents:
	cargo xtask agents linux

darwin-agents:
	cargo xtask agents darwin

windows-agents:
	cargo xtask agents windows

clean:
	cargo xtask clean

# ─── Install target (used by packaging scripts) ───
#
# Unix packaging only (PREFIX/.desktop/hicolor). Linux agents are
# `newt-agent`; consumed from the `agents/` tree `cargo xtask agents`
# produced.

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
