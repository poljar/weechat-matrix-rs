WEECHAT_HOME ?= $(HOME)/.weechat
PREFIX ?= $(WEECHAT_HOME)

.PHONY: install install-dir phony lint

target/debug/libmatrix.so: src/lib.rs src/server.rs src/room_buffer.rs
	cargo build

install: install-dir target/debug/libmatrix.so
	install -m644  target/debug/libmatrix.so $(DESTDIR)$(PREFIX)/plugins/matrix.so

install-dir:
	install -d $(DESTDIR)$(PREFIX)/plugins

lint:
	cargo clippy

