# See https://weechat.org/files/doc/weechat/stable/weechat_user.en.html#xdg_directories
XDG_DATA_HOME ?= $(HOME)/.local/share
WEECHAT_DATA_DIR ?= $(XDG_DATA_HOME)/weechat

SOURCES := $(wildcard src/*.rs src/commands/*.rs Cargo.lock)

.PHONY: install install-dir lint

target/debug/libmatrix.so: $(SOURCES)
	cargo build

install: install-dir target/debug/libmatrix.so
	install -m644  target/debug/libmatrix.so $(DESTDIR)$(WEECHAT_DATA_DIR)/plugins/matrix.so

install-dir:
	install -d $(DESTDIR)$(WEECHAT_DATA_DIR)/plugins

lint:
	cargo clippy
