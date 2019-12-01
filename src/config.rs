use crate::PLUGIN_NAME;
use weechat::Weechat;

pub struct Config(weechat::Config);

impl Config {
    pub fn new(weechat: &Weechat) -> Config {
        let config = weechat
            .config_new(PLUGIN_NAME, || {})
            .expect("Can't create new config");

        Config(config)
    }
}
