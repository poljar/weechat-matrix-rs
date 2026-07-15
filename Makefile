# See https://weechat.org/files/doc/weechat/stable/weechat_user.en.html#xdg_directories
XDG_DATA_HOME ?= $(HOME)/.local/share
WEECHAT_DATA_DIR ?= $(XDG_DATA_HOME)/weechat
DEB_HOST_MULTIARCH ?= $(shell dpkg-architecture -qDEB_HOST_MULTIARCH 2>/dev/null)
ifeq ($(DEB_HOST_MULTIARCH),)
WEECHAT_PLUGIN_DIR ?= /usr/lib/weechat/plugins
else
WEECHAT_PLUGIN_DIR ?= /usr/lib/$(DEB_HOST_MULTIARCH)/weechat/plugins
endif

SOURCES := $(wildcard src/*.rs src/bar_items/*.rs src/commands/*.rs src/room/*.rs Cargo.lock)

PROFILE ?= release

.PHONY: install install-user uninstall uninstall-user install-dir install-user-dir lint check all help deb

all: help

help: ## Print this help message
	@grep -E '^[a-zA-Z._-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*## "}; {printf "\033[36m%-30s\033[0m %s\n", $$1, $$2}'; printf '\nAppend PROFILE=debug to the install and deb targets for debug builds\n\n'

target/debug/libmatrix.so: $(SOURCES) ## Build plugin in dev profile
	cargo build

target/release/libmatrix.so: $(SOURCES) ## Build plugin release profile
	cargo build --release

install: install-dir target/$(PROFILE)/libmatrix.so ## Install plugin systemwide
	install -m644  target/$(PROFILE)/libmatrix.so $(DESTDIR)$(WEECHAT_PLUGIN_DIR)/matrix.so

install-user: install-user-dir target/$(PROFILE)/libmatrix.so ## Install plugin to user WeeChat dir
	install -m644  target/$(PROFILE)/libmatrix.so $(DESTDIR)$(WEECHAT_DATA_DIR)/plugins/matrix.so

uninstall: ## Remove systemwide plugin
	rm -f $(DESTDIR)$(WEECHAT_PLUGIN_DIR)/matrix.so

uninstall-user: ## Remove plugin from user WeeChat dir
	rm -f $(DESTDIR)$(WEECHAT_DATA_DIR)/plugins/matrix.so

install-dir:
	install -d $(DESTDIR)$(WEECHAT_PLUGIN_DIR)

install-user-dir:
	install -d $(DESTDIR)$(WEECHAT_DATA_DIR)/plugins

lint: ## Lint issues with clippy
	cargo clippy

check: ## Run test suite
	cargo test --release

# Get the base package version from Cargo.toml (fallback when no git tags are found)
CARGO_VERSION := $(shell grep -m1 '^version = ' Cargo.toml | sed 's/version = "\(.*\)"/\1/')

deb: target/$(PROFILE)/libmatrix.so ## Build a .deb package with version from git describe
	DEB_VERSION=$$( \
	  desc=$$(git describe --tags --always --dirty --match '[0-9]*' 2>/dev/null); \
	  if [ -z "$$desc" ]; then \
	    echo "$(CARGO_VERSION)+unknown"; \
	  else \
	    case "$$desc" in \
	      *-dirty) dirty=".dirty"; desc="$${desc%-dirty}" ;; \
	      *) dirty="" ;; \
	    esac; \
	    case "$$desc" in \
	      *.*) ;; \
	      *) desc="$(CARGO_VERSION)+git.$$desc" ;; \
	    esac; \
	    desc=$$(echo "$$desc" | sed 's/-\([0-9][0-9]*\)-g\([0-9a-f][0-9a-f]*\)/+\1.\2/'); \
	    echo "$$desc$$dirty"; \
	  fi \
	); \
	cargo deb --no-build --deb-version "$$DEB_VERSION"
	@echo ""
	@echo "Package built: target/debian/weechat-matrix-rs_$${DEB_VERSION}*.deb"
	@echo "Install with: sudo dpkg -i target/debian/weechat-matrix-rs_$${DEB_VERSION}*.deb"
