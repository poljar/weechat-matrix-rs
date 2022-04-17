WEECHAT_HOME ?= $(HOME)/.weechat
PREFIX ?= $(WEECHAT_HOME)

SOURCES := $(wildcard src/*.rs src/commands/*.rs Cargo.lock)

.PHONY: install install-release install-debug install-dir lint release debug

debug: target/debug/libmatrix.so
release: target/release/libmatrix.so

target/release/libmatrix.so: $(SOURCES)
	cargo build --release

target/debug/libmatrix.so: $(SOURCES)
	cargo build

install-release: install-dir release
	install -m644  target/release/libmatrix.so $(DESTDIR)$(PREFIX)/plugins/matrix.so

install-debug: install-dir debug
	install -m644  target/debug/libmatrix.so $(DESTDIR)$(PREFIX)/plugins/matrix.so

install-dir:
	install -d $(DESTDIR)$(PREFIX)/plugins

install: install-release

lint:
	cargo clippy
