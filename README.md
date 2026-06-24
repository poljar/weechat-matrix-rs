[![build-test-release](https://github.com/poljar/weechat-matrix-rs/actions/workflows/release.yml/badge.svg?event=push)](https://github.com/poljar/weechat-matrix-rs/actions/workflows/release.yml)
[![#weechat-matrix](https://img.shields.io/badge/matrix-%23weechat--matrix:termina.org.uk-blue.svg?style=flat-square)](https://matrix.to/#/!twcBhHVdZlQWuuxBhN:termina.org.uk?via=termina.org.uk&via=matrix.org)
[![license](https://img.shields.io/badge/license-ISC-blue.svg?style=flat-square)](https://github.com/poljar/weechat-matrix-rs/blob/master/LICENSE)

# What is weechat-matrix?

[Weechat](https://weechat.org/) is an extensible chat client.

[Matrix](https://matrix.org/blog/home) is an open network for secure,
decentralized communication.

weechat-matrix-rs is a Rust plugin for Weechat that lets Weechat communicate
over the Matrix protocol. This is a Rust rewrite of the
[weechat-matrix](https://github.com/poljar/weechat-matrix) Python script.

# Project status

This project is a work in progress and doesn't do much yet. It can connect
to a Matrix server and send messages.

If you are interested in helping out take a look at the issue tracker.

# Build

Install Rust and the native build dependencies first. On Debian or Ubuntu a
typical build environment needs:

    sudo apt install build-essential clang libclang-dev libsqlite3-dev pkg-config

Other distributions use different package names, but the important native
pieces are a C/C++ compiler, `clang`/`libclang`, and SQLite development headers.
The `weechat-sys` dependency has a bundled WeeChat plugin header
(`weechat-plugin.h`) fallback. If you want to build against the exact WeeChat
API installed on your system, install the matching development package
(`weechat-dev` on Debian/Ubuntu) or point `WEECHAT_PLUGIN_FILE` at the full
path to that header:

    export WEECHAT_PLUGIN_FILE=/usr/include/weechat/weechat-plugin.h

After the dependencies are installed the plugin can be compiled with:

    cargo build --release

If you are developing on weechat-matrix-rs, use debug builds which are faster at
the expense of plugin performance:

    cargo build

On Linux this creates a `libmatrix.so` file in the `target/release/`
(`target/debug` for dev builds) folder. Rename it to `matrix.so` and copy it to
your WeeChat plugin directory.

`make install` uses WeeChat's XDG data directory by default:

    make install

This installs the release build to:

    ${XDG_DATA_HOME:-$HOME/.local/share}/weechat/plugins/matrix.so

Use `PROFILE=debug` for a debug build:

    make install PROFILE=debug

On older WeeChat setups the plugin directory may instead live under
`$WEECHAT_HOME/plugins`, commonly `~/.weechat/plugins/`. Create the directory if
needed and copy the renamed plugin there.

On macOS the built library extension is usually `.dylib`. WeeChat may need this
extension enabled before it loads the plugin:

    /set weechat.plugin.extension ".so,.dll,.dylib"

# Loading the plugin

Restart WeeChat after installing the plugin, or load it manually:

    /plugin load matrix

Check that it is loaded with:

    /plugin list

If WeeChat cannot find the plugin, check that the file is named `matrix.so`
(`matrix.dylib` on macOS), that it is in a directory from WeeChat's plugin
search path, and that `.so`/`.dylib` is enabled in
`weechat.plugin.extension`.

# Configuration

Configuration is completed primarily through the Weechat interface. First start
WeeChat, make sure the plugin is loaded, and then issue the following commands
_(replace the placeholders in brackets [] with your own details)_:

1. Add a server _(make sure the url includes the scheme e.g. 'https://matrix.org')_:

       /matrix server add [server-name] [server-url]

2. Set your username and password:

       /set matrix-rust.server.[server-name].username [username]
       /set matrix-rust.server.[server-name].password [password]

3. Now try to connect. The first connection can take a few minutes while the
   client syncs the account:

       /matrix connect [server-name]

4. Automatically connect to the server:

       /set matrix-rust.server.[server-name].autoconnect on

5. If everything works, save the configuration:

       /save


# Helpful Commands

`/help matrix` will print information about the `/matrix` command.

`/matrix help [command]` will print information for subcommands, such as `/matrix help server`.

Room buffers accept normal message input after `/matrix connect [server-name]`
has completed.
