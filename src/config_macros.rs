use weechat::config::{
    BooleanOptionSettings, Conf, ConfigOption, ConfigSection,
    ConfigSectionSettings, IntegerOptionSettings, OptionChanged, SectionHandle,
    SectionHandleMut, SectionReadCallback, StringOptionSettings, ColorOptionSettings,
};
use weechat::Weechat;

use strum::VariantNames;

macro_rules! string_create {
    ($option_name:ident, $description:literal, $default:literal) => {
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
    };
}

macro_rules! color_create {
    ($option_name:ident, $description:literal, $default:literal) => {
        paste::item! {
            fn [<create_option_ $option_name>](section: &mut SectionHandleMut) {
                let option_name = stringify!($option_name);
                let option_settings = ColorOptionSettings::new(option_name)
                    .description($description)
                    .default_value($default);

                section.new_color_option(option_settings)
                    .expect(&format!("Can't create option {}", option_name));
            }
        }
    };
}

macro_rules! bool_create {
    ($option_name:ident, $description:literal, $default:literal) => {
        paste::item! {
            fn [<create_option_ $option_name>](section: &mut SectionHandleMut) {
                let option_name = stringify!($option_name);
                let option_settings = BooleanOptionSettings::new(option_name)
                    .description($description)
                    .default_value($default);

                section.new_boolean_option(option_settings)
                    .expect(&format!("Can't create option {}", option_name));
            }
        }
    };
}

macro_rules! integer_create {
    ($option_name:ident, $description:literal, $default:literal, $min:literal, $max:literal) => {
        paste::item! {
            fn [<create_option_ $option_name>](section: &mut SectionHandleMut) {
                let option_name = stringify!($option_name);

                let option_settings = IntegerOptionSettings::new(option_name)
                    .description($description)
                    .default_value($default)
                    .min($min)
                    .max($max);

                section.new_integer_option(option_settings)
                    .expect(&format!("Can't create option {}", option_name));
            }
        }
    };
}

macro_rules! enum_create {
    ($option_name:ident, $description:literal, $out_type:ty) => {
        paste::item! {
            fn [<create_option_ $option_name>](section: &mut SectionHandleMut) {
                let mut string_values: Vec<String> = Vec::new();

                for value in $out_type::VARIANTS {
                    string_values.push(value.to_string());
                }

                let default_value = $out_type::default();

                let option_name = stringify!($option_name);
                let option_settings = IntegerOptionSettings::new(option_name)
                    .description($description)
                    .default_value(default_value as i32)
                    .string_values(string_values);

                section.new_integer_option(option_settings)
                    .expect(&format!("Can't create option {}", option_name));
            }
        }
    };
}

macro_rules! option_getter {
    ($option_type:ident, $option_name:ident, $output_type:ty) => {
        paste::item! {
            pub fn [<$option_name>](&self) -> $output_type {
                let option_name = stringify!($option_name);

                if let ConfigOption::[<$option_type>](o) = self.0.search_option(option_name)
                    .expect(&format!("Couldn't find option {} in section {}",
                                     option_name, self.0.name()))
                {
                    $output_type::from(o.value())
                } else {
                    panic!("Incorect option type for option {} in section {}",
                           option_name, self.0.name());
                }
            }
        }
    };
}

macro_rules! option {
    (String, $option_name:ident, $description:literal, $default:literal $(,)?) => {
        string_create!($option_name, $description, $default);
        option_getter!(String, $option_name, String);
    };

    (Color, $option_name:ident, $description:literal, $default:literal $(,)?) => {
        color_create!($option_name, $description, $default);
        option_getter!(Color, $option_name, String);
    };

    (bool, $option_name:ident, $description:literal, $default:literal $(,)?) => {
        bool_create!($option_name, $description, $default);
        option_getter!(Boolean, $option_name, bool);
    };

    (Integer, $option_name:ident, $description:literal, $default:literal, $min:literal..$max:literal $(,)?) => {
        integer_create!($option_name, $description, $default, $min, $max);
        option_getter!(Integer, $option_name, i64);
    };

    (Enum, $option_name:ident, $description:literal, $out_type:ty $(,)?) => {
        enum_create!($option_name, $description, $out_type);
        option_getter!(Integer, $option_name, $out_type);
    };

    (EvaluatedString, $option_name:ident, $description:literal, $default:literal $(,)?) => {
        string_create!($option_name, $description, $default);

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
    ($section:ident { $($option_name:ident: $option_type:ident {$($option:tt)*}), * $(,)? }) => {
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
    ($(Section $section:ident { $($option:tt)* }), * $(,)?) => {
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
            section!($section { $($option)* });
        )*
    }
}
