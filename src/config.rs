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
    Conf, ConfigOption, ConfigSection, ConfigSectionSettings, OptionChanged,
    SectionHandle, SectionHandleMut, SectionReadCallback, StringOptionSettings,
};
use weechat::Weechat;

use std::cell::{Ref, RefCell, RefMut};
use std::ops::{Deref, DerefMut};
use std::rc::Rc;

#[derive(Clone)]
pub struct ConfigHandle {
    inner: Rc<RefCell<Config>>,
    servers: Servers,
}

macro_rules! option {
    (StringOption, $option_name:ident, $description:literal, $default:literal) => {
        paste::item! {
            fn [<create_option_ $option_name>](section: &mut SectionHandleMut) {
                let option_name = stringify!($option_name);
                let option_settings = StringOptionSettings::new(option_name)
                    .description($description)
                    .default_value($default);

                section.new_string_option(option_settings)
                    .expect(&format!("Can't create option {}", option_name));
            }
        }

        paste::item! {
            pub fn [<$option_name>](&self) -> String {
                let option_name = stringify!($option_name);

                let option = self.0.search_option(option_name)
                    .expect(&format!("Couldn't find option {} in section {}",
                                     option_name, self.0.name()));

                if let ConfigOption::String(o) = option {
                    o.value().to_string()
                } else {
                    panic!("Incorect option type for option {} in section {}",
                           option_name, self.0.name());
                }
            }
        }
     };

     (EvaluatedStringOption, $option_name:ident, $description:literal, $default:literal) => {
        paste::item! {
            fn [<create_option_ $option_name>](section: &mut SectionHandleMut) {
                let option_name = stringify!($option_name);
                let option_settings = StringOptionSettings::new(option_name)
                    .description(&format!("{} (note: the content is evaluated, see /help eval)", $description))
                    .default_value($default);

                section.new_string_option(option_settings)
                    .expect(&format!("Can't create option {}", option_name));
            }
        }

        paste::item! {
            pub fn [<$option_name>](&self) -> String {
                let option_name = stringify!($option_name);

                let option = self.0.search_option(option_name)
                    .expect(&format!("Couldn't find option {} in section {}",
                                     option_name, self.0.name()));

                if let ConfigOption::String(o) = option {
                    Weechat::eval_string_expression(&o.value())
                        .expect(&format!(
                            "Can't evaluate string expression for option {} in section {}",
                            option_name,
                            self.0.name())
                        )
                } else {
                    panic!("Incorect option type for option {} in section {}",
                           option_name, self.0.name());
                }
            }
        }
     };
}

macro_rules! section {
    ($section:ident { $({$option_type:ident, $option_name:ident, $($option:tt)*}), * }) => {
        paste::item! {
            pub struct [<$section:camel Section>]<'a>(SectionHandle<'a>);

            impl<'a> [<$section:camel Section>]<'a> {
                pub fn create(config: &mut Config) {
                    let section_settings = ConfigSectionSettings::new(stringify!($section));

                    let mut $section = config.new_section(section_settings)
                        .expect(&format!("Can't create config section {}", stringify!($section)));

                    [<$section:camel Section>]::create_options(&mut $section);
                }

                pub fn create_options(section: &mut SectionHandleMut) {
                    $(
                        [<$section:camel Section>]::[<create_option_ $option_name>](section);
                    )*
                }

                $(
                    option!($option_type, $option_name, $($option)*);
                )*
            }
        }
    }
}

macro_rules! config {
    ($(Section $section:ident { $({$($option:tt)*}), * }), *) => {
        pub struct Config(weechat::config::Config);

        impl Deref for Config {
            type Target = weechat::config::Config;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl DerefMut for Config {
            fn deref_mut(&mut self) -> &mut Self::Target {
                &mut self.0
            }
        }

        impl Config {
            fn new(config: weechat::config::Config) -> Self {
                let mut config = Config(config);
                config.create_sections();

                config
            }

            paste::item! {
                fn create_sections(&mut self) {
                    $(
                        paste::expr! { [<$section:camel Section>]::create(self) };
                    )*
                }
            }

            paste::item! {
                $(
                    pub fn $section(&self) -> [<$section:camel Section>] {
                        let section_name = stringify!($section);
                        let section = self.0.search_section(section_name)
                            .expect(&format!("Couldn't find section {}", section_name));

                        paste::item! { [<$section:camel Section>](section) }
                    }
                )*
            }
        }

        $(
            section!($section { $({$($option)*}), * });
        )*
    }
}

config!(
    Section look {
        {StringOption, encrypted_room_sign,
         "A sign that is used to show that the current room is encrypted",
         "🔒"}
    }
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
