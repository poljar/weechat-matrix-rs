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

After Rust is installed the plugin can be compiled with:

    cargo build

On Linux this creates a `libmatrix.so` file in the `target/debug/` folder, this
file needs to be renamed to `matrix.so` and copied to your Weechat plugin
directory. A plugin directory can be created in your `$WEECHAT_HOME` folder, by
default `.weechat/plugins/`.

Alternatively, `make install` will build and install the plugin in your
`$WEECHAT_HOME` as well.

# Usage

`/help matrix` displays all Matrix-specific commands along with brief descriptions. To connect to a new server, follow these steps:

1. Add a server using `/matrix server add [server-name] [server-url]`. Make sure the url includes the scheme (eg 'https://matrix.org').
2. Add username and password: `/set matrix-rust.server.[server-name].username [username]` and `/set matrix-rust.server.[server-name].password [password]`.
3. Connect to server using `/matrix connect [server-name]`. This may take several minutes, be patient.
