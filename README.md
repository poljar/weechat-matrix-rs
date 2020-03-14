[![Build Status](https://img.shields.io/travis/poljar/weechat-matrix-rs.svg?style=flat-square)](https://travis-ci.org/poljar/weechat-matrix-rs)
[![#weechat-matrix](https://img.shields.io/badge/matrix-%23weechat--matrix:termina.org.uk-blue.svg?style=flat-square)](https://matrix.to/#/!twcBhHVdZlQWuuxBhN:termina.org.uk?via=termina.org.uk&via=matrix.org)
[![license](https://img.shields.io/badge/license-ISC-blue.svg?style=flat-square)](https://github.com/poljar/weechat-matrix-rs/blob/master/LICENSE)

# What is Weechat-Matrix?

[Weechat](https://weechat.org/) is an extensible chat client.

[Matrix](https://matrix.org/blog/home) is an open network for secure,
decentralized communication.

[weechat-matrix-rs](https://github.com/poljar/weechat-matrix-rs/) is a Rust
plugin for Weechat that lets Weechat communicate over the Matrix protocol. This
is a Rust rewrite of the [weechat-matrix](https://github.com/poljar/weechat-matrix)
Python script.

# Project Status

This project is a work in progress and doesn't do much yet. It can connect
to a Matrix server and send messages.

If you are interested in helping out take a look at the issue tracker.

# Build

To build this project a
[nightly](https://github.com/rust-lang/rustup#working-with-nightly-rust)
version of Rust is required.

After Rust is installed the plugin can be compiled with:

    cargo build

On Linux this creates a `libmatrix.so` file in the `target/debug/` folder, this
file needs to be renamed to `matrix.so` and copied to your Weechat plugin
directory. A plugin directory can be created in your `$WEECHAT_HOME` folder, by
default `.weechat/plugins/`.

Alternatively `make install` will build and install the plugin in your
`$WEECHAT_HOME` as well.
