# See https://weechat.org/files/doc/weechat/stable/weechat_user.en.html#xdg_directories
XDG_DATA_HOME ?= $(HOME)/.local/share
WEECHAT_DATA_DIR ?= $(XDG_DATA_HOME)/weechat

SOURCES := $(wildcard src/*.rs src/bar_items/*.rs src/commands/*.rs src/room/*.rs Cargo.lock)

PROFILE ?= release

.PHONY: install install-dir lint all help

all: help

help: ## Print this help message
	@grep -E '^[a-zA-Z._-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*## "}; {printf "\033[36m%-30s\033[0m %s\n", $$1, $$2}'

target/debug/libmatrix.so: $(SOURCES) ## Build plugin in dev profile
	cargo build

target/release/libmatrix.so: $(SOURCES) ## Build plugin release profile
	cargo build --release

install: install-dir target/$(PROFILE)/libmatrix.so ## Install plugin to weechat dir
	install -m644  target/$(PROFILE)/libmatrix.so $(DESTDIR)$(WEECHAT_DATA_DIR)/plugins/matrix.so

install-dir: ## Create plugins directory
	install -d $(DESTDIR)$(WEECHAT_DATA_DIR)/plugins

lint: ## Lint issues with clippy
	cargo clippy
