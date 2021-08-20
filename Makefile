WEECHAT_HOME ?= $(HOME)/.weechat
PREFIX ?= $(WEECHAT_HOME)

.PHONY: install install-dir lint target/debug/libmatrix.so

target/debug/libmatrix.so:
	cargo build --release

install: install-dir target/debug/libmatrix.so
	install -m644  target/release/libmatrix.so $(DESTDIR)$(PREFIX)/plugins/matrix.so

install-dir:
	install -d $(DESTDIR)$(PREFIX)/plugins

lint:
	cargo clippy
