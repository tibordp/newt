VERSION := $(shell cargo metadata --format-version 1 --no-deps | python3 -c "import sys,json; [print(p['version']) for p in json.load(sys.stdin)['packages'] if p['name']=='newt-agent']")

DIST := agents

LINUX_TARGETS := x86_64-unknown-linux-musl aarch64-unknown-linux-musl
DARWIN_TARGETS := aarch64-apple-darwin x86_64-apple-darwin

# sha256sum on Linux, shasum -a 256 on macOS
SHA256 := $(shell command -v sha256sum 2>/dev/null || echo "shasum -a 256")

CROSS_TARGETS := aarch64-unknown-linux-musl

.PHONY: agents linux-agents darwin-agents cross-agents clean

agents: linux-agents darwin-agents

linux-agents: $(addprefix $(DIST)/,$(addsuffix /newt-agent,$(LINUX_TARGETS)))

darwin-agents: $(addprefix $(DIST)/,$(addsuffix /newt-agent,$(DARWIN_TARGETS)))

cross-agents: $(addprefix $(DIST)/,$(addsuffix /newt-agent.cross,$(CROSS_TARGETS)))

# Build via cross (Docker-based, no local toolchain needed)
$(DIST)/%/newt-agent.cross: FORCE
	cross build --release --target $* -p newt-agent
	@mkdir -p $(dir $@)
	cp target/$*/release/newt-agent $(DIST)/$*/newt-agent
	$(SHA256) $(DIST)/$*/newt-agent > $(DIST)/$*/newt-agent.sha256

# All targets via cargo (install musl toolchains locally)
$(DIST)/%/newt-agent: FORCE
	cargo build --release --target $* -p newt-agent
	@mkdir -p $(dir $@)
	cp target/$*/release/newt-agent $@
	$(SHA256) $@ > $@.sha256

clean:
	rm -rf $(DIST)

FORCE:
