# See https://weechat.org/files/doc/weechat/stable/weechat_user.en.html#xdg_directories
XDG_DATA_HOME ?= $(HOME)/.local/share
WEECHAT_DATA_DIR ?= $(XDG_DATA_HOME)/weechat

SOURCES := $(wildcard src/*.rs src/bar_items/*.rs src/commands/*.rs src/room/*.rs Cargo.lock)

PROFILE ?= release

.PHONY: install install-dir lint all help deb

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

# Get the base package version from Cargo.toml (fallback when no git tags are found)
CARGO_VERSION := $(shell grep -m1 '^version = ' Cargo.toml | sed 's/version = "\(.*\)"/\1/')

deb: target/$(PROFILE)/libmatrix.so ## Build a .deb package with version from git describe
	DEB_VERSION=$$( \
	  tag=$$(git describe --tags --abbrev=0 --match '[0-9]*' 2>/dev/null || true); \
	  short=$$(git rev-parse --short HEAD 2>/dev/null || true); \
	  if [ -z "$$short" ]; then \
	    echo "$(CARGO_VERSION)+unknown"; \
	  elif [ -z "$$tag" ]; then \
	    count=$$(git rev-list --count HEAD 2>/dev/null || echo 0); \
	    dirty=$$(git diff-index --quiet HEAD -- 2>/dev/null || echo ".dirty"); \
	    echo "$(CARGO_VERSION)+git.$$count.g$$short$$dirty"; \
	  else \
	    desc=$$(git describe --tags --dirty --match '[0-9]*' 2>/dev/null); \
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
	@echo "Package built: target/debian/weechat-matrix_$${DEB_VERSION}*.deb"
	@echo "Install with: sudo dpkg -i target/debian/weechat-matrix_$${DEB_VERSION}*.deb"
