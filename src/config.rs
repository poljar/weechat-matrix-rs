//! Configuration module
//!
//! This is the global plugin configuration.
//!
//! Configuration options should be split out into different sections:
//!
//! * network
//! * look
//! * color
//! * server
//!
//! The server config options are added in the server.rs file.
//!
//! The config options created here will be alive as long as the plugin is
//! loaded so they don't need to be freed manually. The drop implementation of
//! the section will do so.

use std::{
    cell::{Ref, RefCell, RefMut},
    rc::Rc,
};

use strum_macros::EnumVariantNames;
use weechat::{
    config,
    config::{
        Conf, ConfigOption, ConfigSection, ConfigSectionSettings,
        IntegerOptionSettings, OptionChanged, SectionReadCallback,
    },
    Weechat,
};

use crate::{MatrixServer, Servers};

#[derive(EnumVariantNames)]
#[strum(serialize_all = "kebab_case")]
pub enum RedactionStyle {
    StrikeThrough,
    Delete,
    Notice,
}

impl Default for RedactionStyle {
    fn default() -> Self {
        RedactionStyle::StrikeThrough
    }
}

impl From<i32> for RedactionStyle {
    fn from(value: i32) -> Self {
        match value {
            0 => RedactionStyle::StrikeThrough,
            1 => RedactionStyle::Delete,
            2 => RedactionStyle::Notice,
            _ => unreachable!(),
        }
    }
}

#[derive(EnumVariantNames)]
#[strum(serialize_all = "kebab_case")]
pub enum ServerBuffer {
    MergeWithCore,
    MergeWithoutCore,
    Independent,
}

impl Default for ServerBuffer {
    fn default() -> Self {
        ServerBuffer::MergeWithCore
    }
}

impl From<i32> for ServerBuffer {
    fn from(value: i32) -> Self {
        match value {
            0 => ServerBuffer::MergeWithCore,
            1 => ServerBuffer::MergeWithoutCore,
            2 => ServerBuffer::Independent,
            _ => unreachable!(),
        }
    }
}

config!(
    "matrix-rust",
    Section look {
        encrypted_room_sign: String {
            // Description.
            "A sign that is used to show that the current room is encrypted",
            // Default value.
            "🔒",
        },

        public_room_sign: String {
            // Description.
            "A sign indicating that the current room is public",
            // Default value.
            "🌍",
        },

        busy_sign: String {
            // Description.
            "A sign that is used to show that the client is busy, \
                e.g. when room history is being fetched",
            // Default value.
            "⏳",
        },

        local_echo: bool {
            // Description
            "Should the sending message be printed out before the server \
             confirms the reception of the message",
             // Default value
             true,
        },

        redaction_style: Enum {
            // Description
            "The style that should be used when a message needs to be redacted",
            RedactionStyle,
        },
    },
    Section network {
        debug_buffer: bool {
            // Description
            "Use a separate buffer for debug logs",
            // Default value.
            false,
        },
    }
);

/// A wrapper for our config struct that can be cloned around.
#[derive(Clone)]
pub struct ConfigHandle {
    pub inner: Rc<RefCell<Config>>,
    servers: Servers,
}

impl ConfigHandle {
    /// Create a new config and wrap it in our config handle.
    pub fn new(servers: &Servers) -> ConfigHandle {
        let config = Config::new().expect("Can't create new config");

        let config = ConfigHandle {
            inner: Rc::new(RefCell::new(config)),
            servers: servers.clone(),
        };

        // The server section is special since it has a custom section read and
        // write implementations to support subsections for every configured
        // server.
        let server_section_options = ConfigSectionSettings::new("server")
            .set_write_callback(
                |_weechat: &Weechat,
                 config: &Conf,
                 section: &mut ConfigSection| {
                    config.write_section(section.name());
                    for option in section.options() {
                        config.write_option(option);
                    }
                },
            )
            .set_read_callback(config.clone());

        {
            let mut config_borrow = config.borrow_mut();

            config_borrow
                .new_section(server_section_options)
                .expect("Can't create server section");

            let mut look_section = config_borrow.look_mut();

            let servers = servers.clone();

            let settings = IntegerOptionSettings::new("server_buffer")
                .description("Should the server buffer be merged with other buffers or independent")
                .set_change_callback(move |_, _| {
                    for server in servers.borrow().values() {
                        server.merge_server_buffers();
                    }
                })
                .default_value(ServerBuffer::default() as i32)
                .string_values(
                    ServerBuffer::VARIANTS
                        .iter()
                        .map(|v| v.to_string())
                        .collect::<Vec<String>>(),
                );

            look_section
                .new_integer_option(settings)
                .expect("Can't create server buffers option");
        }

        config
    }

    pub fn borrow(&self) -> Ref<'_, Config> {
        self.inner.borrow()
    }

    pub fn borrow_mut(&self) -> RefMut<'_, Config> {
        self.inner.borrow_mut()
    }
}

impl<'a> LookSection<'a> {
    pub fn server_buffer(&self) -> ServerBuffer {
        if let ConfigOption::Integer(o) =
            self.search_option("server_buffer").unwrap()
        {
            ServerBuffer::from(o.value())
        } else {
            panic!("Server buffer option has the wrong type");
        }
    }
}

impl SectionReadCallback for ConfigHandle {
    fn callback(
        &mut self,
        _: &Weechat,
        _: &Conf,
        section: &mut ConfigSection,
        option_name: &str,
        option_value: &str,
    ) -> OptionChanged {
        if option_name.is_empty() {
            return OptionChanged::Error;
        }

        let option_args: Vec<&str> = option_name.splitn(2, '.').collect();

        if option_args.len() != 2 {
            return OptionChanged::Error;
        }

        let server_name = option_args[0];

        // We are reading the config, if the server doesn't yet exists
        // we need to create it before setting the option and running
        // the option change callback.
        if !self.servers.contains(server_name) {
            let server = MatrixServer::new(
                server_name,
                &self,
                section,
                self.servers.clone(),
            );
            self.servers.insert(server);
        }

        let option = section.search_option(option_name);

        if let Some(o) = option {
            o.set(option_value, true)
        } else {
            OptionChanged::NotFound
        }
    }
}
