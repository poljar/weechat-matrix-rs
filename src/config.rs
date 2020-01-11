use crate::{MatrixServer, Servers};
use weechat::config::{
    Conf, ConfigSection, ConfigSectionSettings, OptionChanged,
};
use weechat::Weechat;

use std::cell::{Ref, RefCell, RefMut};
use std::rc::{Rc, Weak};

#[derive(Clone)]
pub struct Config(Rc<RefCell<weechat::config::Config>>);

#[derive(Clone, Default)]
pub struct ConfigHandle(Weak<RefCell<weechat::config::Config>>);

impl ConfigHandle {
    pub fn upgrade(&self) -> Config {
        Config(
            self.0
                .upgrade()
                .expect("Config got deleted before plugin unload"),
        )
    }
}

fn server_write_cb(
    _weechat: &Weechat,
    config: &Conf,
    section: &mut ConfigSection,
) {
    config.write_section(section.name());

    for option in section.options() {
        config.write_option(option);
    }
}

impl Config {
    pub fn new(weechat: &Weechat, servers: &Servers) -> Config {
        let mut config = weechat
            .config_new("matrix-rust")
            .expect("Can't create new config");

        let servers = servers.clone_weak();

        let config = Config(Rc::new(RefCell::new(config)));

        let weak_config = config.clone_weak();

        let server_section_options = ConfigSectionSettings::new("server")
            .set_write_callback(server_write_cb)
            .set_read_callback(
                move |weechat, _config, section, option_name, value| {
                    let config = weak_config.clone();
                    let servers = servers.clone();

                    if option_name.is_empty() {
                        return OptionChanged::Error;
                    }

                    let option_args: Vec<&str> =
                        option_name.splitn(2, '.').collect();

                    if option_args.len() != 2 {
                        return OptionChanged::Error;
                    }

                    let server_name = option_args[0];
                    let option = option_args[1];

                    {
                        let servers = servers.upgrade();
                        let mut servers_borrow = servers.borrow_mut();

                        // We are reading the config, if the server doesn't yet exists
                        // we need to create it before setting the option and running
                        // the option change callback.
                        if !servers_borrow.contains_key(server_name) {
                            let config = config.upgrade();
                            let server = MatrixServer::new(
                                server_name,
                                &config,
                                section,
                            );
                            servers_borrow
                                .insert(server_name.to_owned(), server);
                        }
                    }

                    let option = section.search_option(option_name);

                    if let Some(o) = option {
                        o.set(value, true)
                    } else {
                        OptionChanged::NotFound
                    }
                },
            );

        {
            let mut config_borrow = config.borrow_mut();

            config_borrow
                .new_section(server_section_options)
                .expect("Can't create server section");
        }

        config
    }

    pub fn borrow(&self) -> Ref<'_, weechat::config::Config> {
        self.0.borrow()
    }

    pub fn borrow_mut(&self) -> RefMut<'_, weechat::config::Config> {
        self.0.borrow_mut()
    }

    pub fn clone_weak(&self) -> ConfigHandle {
        ConfigHandle(Rc::downgrade(&self.0))
    }
}
