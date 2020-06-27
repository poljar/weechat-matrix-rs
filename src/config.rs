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
use weechat::config::{
    BooleanOptionSettings, Conf, ConfigOption, ConfigSection,
    ConfigSectionSettings, IntegerOptionSettings, OptionChanged, SectionHandle,
    SectionHandleMut, SectionReadCallback, StringOptionSettings, ColorOptionSettings,
};
use weechat::Weechat;

use strum::EnumVariantNames;
use strum::VariantNames;

use std::cell::{Ref, RefCell, RefMut};
use std::ops::{Deref, DerefMut};
use std::rc::Rc;

#[derive(Clone)]
pub struct ConfigHandle {
    inner: Rc<RefCell<Config>>,
    servers: Servers,
}

#[derive(EnumVariantNames)]
#[strum(serialize_all = "kebab_case")]
pub enum Test {
    First,
    Second,
}

impl Default for Test {
    fn default() -> Self {
        Test::First
    }
}

impl From<i32> for Test {
    fn from(value: i32) -> Self {
        match value {
            0 => Test::First,
            1 => Test::Second,
            _ => unreachable!(),
        }
    }
}

config!(
    Section look {
        encrypted_room_sign: String {
            "A sign that is used to show that the current room is encrypted",
            "🔒",
        },

        what: EvaluatedString {
            "test",
            "test2",
        },

        int_test: Integer {
            "test",
            5,
            0..10,
        },

        enum_test: Enum {
            "test",
            Test,
        },

        bool_test: bool {
            "description",
            false,
        },

        color_test: Color {
            "color",
            "darkgray",
        },
    },
);

impl ConfigHandle {
    pub fn new(_weechat: &Weechat, servers: &Servers) -> ConfigHandle {
        let config = Weechat::config_new("matrix-rust")
            .expect("Can't create new config");

        let config = Config::new(config);

        let config = ConfigHandle {
            inner: Rc::new(RefCell::new(config)),
            servers: servers.clone(),
        };

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
