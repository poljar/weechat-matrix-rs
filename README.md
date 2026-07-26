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

It is not an IRC relay or bridge: IRC clients connected through WeeChat's relay
plugin cannot use Matrix rooms as IRC channels. Remote WeeChat interfaces using
WeeChat relay protocols can access plugin buffers, but Matrix features that
rewrite already-printed lines, such as message edits and redactions, depend on
the remote client supporting WeeChat relay line-update events.

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

Use the same WeeChat API headers as the WeeChat binary that will load the
plugin. If WeeChat reports an API mismatch while loading `matrix.so`, rebuild
after installing the matching development package or setting
`WEECHAT_PLUGIN_FILE`. Set `WEECHAT_BUNDLED=1` only when you deliberately want
to build against the bundled fallback header, for example in a controlled test
or package build.

If `bindgen` reports that `stddef.h`, `libclang`, or `libLLVM` is missing, check
that the `clang` command-line tools, C/C++ compiler headers, and matching
`libclang` package are installed. On minimal distributions the library package
alone may not provide the compiler include paths that `bindgen` needs.
Use a coherent `clang`/`libclang`/`libLLVM` set from the same toolchain; copying
only one shared library from another package or SDK can leave `bindgen` loading
an incompatible LLVM library.

After the dependencies are installed the plugin can be compiled with:

    cargo build --release

If you are developing on weechat-matrix-rs, use debug builds which are faster at
the expense of plugin performance:

    cargo build

On Linux this creates a `libmatrix.so` file in the `target/release/`
(`target/debug` for dev builds) folder. Rename it to `matrix.so` and copy it to
your WeeChat plugin directory.

`make install` installs the plugin systemwide:

    sudo make install

On Debian-like multiarch systems this installs the release build to:

    /usr/lib/${DEB_HOST_MULTIARCH}/weechat/plugins/matrix.so

On other systems it defaults to:

    /usr/lib/weechat/plugins/matrix.so

Set `WEECHAT_PLUGIN_DIR` if your distribution uses another system plugin
directory:

    sudo make install WEECHAT_PLUGIN_DIR=/path/to/weechat/plugins

To install only for the current user, use:

    make install-user

This installs the release build to:

    ${XDG_DATA_HOME:-$HOME/.local/share}/weechat/plugins/matrix.so

Use `PROFILE=debug` for a debug build:

    make install-user PROFILE=debug

The matching uninstall targets are:

    sudo make uninstall
    make uninstall-user

On older WeeChat setups the plugin directory may instead live under
`$WEECHAT_HOME/plugins`, commonly `~/.weechat/plugins/`. Create the directory if
needed and copy the renamed plugin there.

On macOS the built library extension is usually `.dylib`. WeeChat may need this
extension enabled before it loads the plugin:

    /set weechat.plugin.extension ".so,.dll,.dylib"

Rename `target/release/libmatrix.dylib` to `matrix.dylib` before copying it to
your WeeChat plugin directory.

# Debian / Ubuntu package

A `.deb` package can be built using `cargo-deb`. This requires
[Rust installed](https://rustup.rs/) (via rustup, the recommended method).

First ensure `cargo-deb` is available:

    cargo install cargo-deb

Then build the package with:

    make deb

The resulting `.deb` package is placed in `target/debian/`. Install it with:

    sudo dpkg -i target/debian/weechat-matrix-rs_*.deb

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

   If the homeserver advertises single sign-on, leave the password empty to use
   it. WeeChat will print a login URL when you connect.

   The SSO callback listens on `127.0.0.1:29325` on the WeeChat host. When
   WeeChat runs on another machine, create an SSH tunnel before connecting so
   the browser's localhost callback reaches it:

       ssh -L 29325:127.0.0.1:29325 user@weechat-host

3. Now try to connect. The first connection can take a few minutes while the
   client syncs the account:

       /matrix connect [server-name]

4. Automatically connect to the server:

       /set matrix-rust.server.[server-name].autoconnect on

5. If everything works, save the configuration:

       /save

To hide join/leave noise from users who have not been active recently, add a WeeChat
filter for Matrix smart-filter tags:

       /filter add matrix_smart * matrix_smart_filter *

The inactivity delay defaults to five minutes and can be changed with:

       /set matrix-rust.look.smart_filter_delay 300000

# Room Status And Verification

The `buffer_modes` bar item shows Matrix room state. Encrypted rooms show
`matrix-rust.look.encrypted_room_sign`; encrypted rooms that still contain
unverified devices also show `matrix-rust.look.encryption_warning_sign`. Public
rooms show `matrix-rust.look.public_room_sign`, and busy rooms show
`matrix-rust.look.busy_sign`.

Interactive verification requests can be accepted, confirmed, or cancelled from
the room or verification buffer:

       /verification accept
       /verification confirm
       /verification cancel

The plugin renders in-room SAS verification requests and shows the emoji or
decimal short authentication string when it is ready.

# Matrix Spaces

Room buffers expose Matrix parent Spaces as WeeChat local variables. Switch to a
room buffer that belongs to a Space and run:

       /buffer listvar

Look for `space`, `space_id`, `spaces`, and `space_ids`. The singular variables
hold the first parent Space name and room id; the plural variables contain all
known parent Spaces as comma-separated lists.

The buflist plugin will not reorganize Matrix rooms under Spaces by itself.
These variables are available to scripts and custom buflist formats. For
example, to prefix Matrix room names with their first parent Space:

       /set buflist.format.name "${if:${space}?${space}/${name}:${name}}"
       /buflist refresh

Remove that prefix again with:

       /unset buflist.format.name

Matrix rooms can belong to more than one Space. WeeChat buffers cannot be shown
as the same room under multiple server-like parents at the same time, so the
plugin exposes all known parent Spaces as metadata instead of moving room
buffers into a fake hierarchy. Matrix Space buffers use a `+` short-name prefix;
ordinary rooms keep the usual room/channel naming rules.

# Storage

The plugin keeps Matrix state and encryption data in
`$WEECHAT_HOME/matrix-rust/<server>/`. It keeps the Matrix SDK event cache in
`$WEECHAT_HOME/matrix-rust/<server>-cache/`.

If the cache grows or causes too much disk I/O, disconnect WeeChat and remove
only the `-cache` directory. Do not remove the main server directory unless you
want to reset stored Matrix state.
Nick prefix colors for Matrix power levels can be changed with:

       /set matrix-rust.color.nick_prefix_admin lightgreen
       /set matrix-rust.color.nick_prefix_moderator lightmagenta
       /set matrix-rust.color.nick_prefix_power yellow

Existing nicklist entries may need a member update or plugin restart before the new colors are visible.


# Helpful Commands

`/help matrix` will print information about the `/matrix` command.

`/matrix help [command]` will print information for subcommands, such as `/matrix help server`.

Room buffers accept normal message input after `/matrix connect [server-name]`
has completed.

`/matrix media download <mxc-uri> [file]` downloads Matrix media through the logged-in client, including homeservers that require authenticated media. It refuses to overwrite an existing file. When `[file]` is omitted, the media id is saved with the configured prefix, which defaults to `matrix-media-`:

       /set matrix-rust.media.download_prefix matrix-media-

The prefix may include a directory and supports `~`, `$XDG_STATE_HOME`, and `${XDG_STATE_HOME}`. Parent directories are created automatically:

       /set matrix-rust.media.download_prefix "$XDG_STATE_HOME/weechat-matrix/media/matrix-media-"
