VERSION := $(shell cargo metadata --format-version 1 --no-deps | python3 -c "import sys,json; [print(p['version']) for p in json.load(sys.stdin)['packages'] if p['name']=='newt-agent']")

DIST := dist/agents

LINUX_TARGETS := x86_64-unknown-linux-musl
# Add more targets as needed:
# LINUX_TARGETS += aarch64-unknown-linux-musl
# DARWIN_TARGETS := aarch64-apple-darwin  # build on macOS only

.PHONY: agents clean

agents: $(addprefix $(DIST)/,$(addsuffix /newt-agent,$(LINUX_TARGETS)))

# All targets via cargo (install musl toolchains locally)
$(DIST)/%/newt-agent: FORCE
	cargo build --release --target $* -p newt-agent
	@mkdir -p $(dir $@)
	cp target/$*/release/newt-agent $@
	sha256sum $@ > $@.sha256

clean:
	rm -rf $(DIST)

FORCE:
