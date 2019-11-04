WEECHAT_HOME ?= $(HOME)/.weechat
PREFIX ?= $(WEECHAT_HOME)

.PHONY: install install-dir phony lint

target/debug/libweechat_matrix.so: src/lib.rs
	cargo build

install: install-dir
	install -m644  target/debug/libweechat_matrix.so $(DESTDIR)$(PREFIX)/plugins/weechat-matrix-rs.so

install-dir:
	install -d $(DESTDIR)$(PREFIX)/plugins

lint:
	cargo clippy

