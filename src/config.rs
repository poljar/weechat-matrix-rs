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

use crate::{MatrixServer, Servers};
use weechat::{
    config,
    config::{
        Conf, ConfigSection, ConfigSectionSettings, OptionChanged,
        SectionReadCallback,
    },
    Weechat,
};

use std::{
    cell::{Ref, RefCell, RefMut},
    rc::Rc,
};

config!(
    "matrix-rust",
    Section look {
        encrypted_room_sign: String {
            // Description.
            "A sign that is used to show that the current room is encrypted",
            // Default value.
            "🔒",
        },

        local_echo: bool {
            // Description
            "Should the sending event be printed out before the server \
             the receipt of the message",
             // Default value
             true,
        }
    },
    Section network {
        debug_buffer: bool {
            // Description
            "Use a separate buffer for debug logs",
            // Default value.
            false,
        }
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

        {
            let mut servers_borrow = self.servers.borrow_mut();

            // We are reading the config, if the server doesn't yet exists
            // we need to create it before setting the option and running
            // the option change callback.
            if !servers_borrow.contains_key(server_name) {
                let server = MatrixServer::new(server_name, &self, section);
                servers_borrow.insert(server_name.to_owned(), server);
            }
        }

        let option = section.search_option(option_name);

        if let Some(o) = option {
            o.set(option_value, true)
        } else {
            OptionChanged::NotFound
        }
    }
}
