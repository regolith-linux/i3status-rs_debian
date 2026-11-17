//! Theme specification

pub mod color;
pub mod separator;

use std::fmt;
use std::ops::{Deref, DerefMut};

use serde::{Deserialize, de};

use crate::errors::*;
use crate::look::{self, Look};
use crate::util;
use crate::widget::State;
use color::Color;
use separator::Separator;

#[derive(Debug, Clone)]
pub struct Theme(pub ThemeInner);

impl Default for Theme {
    fn default() -> Self {
        ThemeUserConfig::default()
            .try_into()
            .unwrap_or_else(|_| Self(ThemeInner::default()))
    }
}

impl Deref for Theme {
    type Target = ThemeInner;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl DerefMut for Theme {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields, default)]
pub struct ThemeInner {
    pub idle_bg: Color,
    pub idle_fg: Color,
    pub info_bg: Color,
    pub info_fg: Color,
    pub good_bg: Color,
    pub good_fg: Color,
    pub warning_bg: Color,
    pub warning_fg: Color,
    pub critical_bg: Color,
    pub critical_fg: Color,
    /// Colour of error messages. Unset, they take `critical_fg`.
    pub error_fg: Option<Color>,
    /// Colour of icons. Unset (`none`), an icon takes its block's colour.
    pub icon_fg: Color,
    pub separator: Separator,
    pub separator_bg: Color,
    pub separator_fg: Color,
    pub alternating_tint_bg: Color,
    pub alternating_tint_fg: Color,
    pub end_separator: Separator,
    pub start_separator: Separator,
    /// Set while the colours follow the session's Regolith look.
    #[serde(skip)]
    pub look: Option<LookColors>,
}

/// What a theme following the Regolith look knows beyond its colours by
/// state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LookColors {
    /// Colour of a battery between 21% and 50%: the look's error colour, or
    /// the warning colour of the theme the look was laid over.
    pub battery_low_fg: Color,
}

impl Theme {
    pub fn get_colors(&self, state: State) -> (Color, Color) {
        match state {
            State::Idle => (self.idle_bg, self.idle_fg),
            State::Info => (self.info_bg, self.info_fg),
            State::Good => (self.good_bg, self.good_fg),
            State::Warning => (self.warning_bg, self.warning_fg),
            State::Critical => (self.critical_bg, self.critical_fg),
        }
    }

    /// The foreground colour of error messages.
    pub fn error_fg(&self) -> Color {
        self.error_fg.unwrap_or(self.critical_fg)
    }

    /// Take the colours of a Regolith look (see [`look`]). Each colour the
    /// look does not define, or defines invalidly, stays as it is.
    ///
    /// Symbols take `i3xrocks.label.color`; readings at rest and for
    /// information `i3xrocks.value.color`, good ones `i3xrocks.nominal`,
    /// warnings `i3xrocks.warning`, critical ones `i3xrocks.critical.color`,
    /// and error messages `i3xrocks.error.color`.
    pub fn apply_look(&mut self, look: &Look) {
        let base_warning_fg = self.warning_fg;
        let base_error_fg = self.error_fg();
        let theme = &mut self.0;
        for (slot, resource) in [
            (&mut theme.icon_fg, look::LABEL_COLOR),
            (&mut theme.idle_fg, look::VALUE_COLOR),
            (&mut theme.info_fg, look::VALUE_COLOR),
            (&mut theme.good_fg, look::NOMINAL_COLOR),
            (&mut theme.warning_fg, look::WARNING_COLOR),
            (&mut theme.critical_fg, look::CRITICAL_COLOR),
        ] {
            if let Some(color) = look.color(resource) {
                *slot = color;
            }
        }
        // Without an error colour of its own, errors keep the theme's
        // colour for them rather than taking the look's critical colour.
        let error_fg = look.color(look::ERROR_COLOR);
        theme.error_fg = Some(error_fg.unwrap_or(base_error_fg));
        theme.look = Some(LookColors {
            battery_low_fg: error_fg.unwrap_or(base_warning_fg),
        });
    }

    pub fn apply_overrides(&mut self, overrides: ThemeOverrides) -> Result<()> {
        let copy = self.clone();

        // Error messages follow an overridden critical colour, unless their
        // own colour is overridden as well.
        if overrides.critical_fg.is_some() && overrides.error_fg.is_none() {
            self.error_fg = None;
        }

        if let Some(separator) = overrides.separator {
            self.separator = separator;
        }
        if let Some(end_separator) = overrides.end_separator {
            self.end_separator = end_separator;
        }
        if let Some(start_separator) = overrides.start_separator {
            self.start_separator = start_separator;
        }

        macro_rules! apply {
            ($prop:tt) => {
                if let Some(color) = overrides.$prop {
                    self.$prop = color.eval(&copy)?;
                }
            };
        }
        apply!(idle_bg);
        apply!(idle_fg);
        apply!(info_bg);
        apply!(info_fg);
        apply!(good_bg);
        apply!(good_fg);
        apply!(warning_bg);
        apply!(warning_fg);
        apply!(critical_bg);
        apply!(critical_fg);
        apply!(icon_fg);
        apply!(separator_bg);
        apply!(separator_fg);
        apply!(alternating_tint_bg);
        apply!(alternating_tint_fg);
        if let Some(color) = overrides.error_fg {
            self.error_fg = Some(color.eval(&copy)?);
        }

        Ok(())
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct ThemeUserConfig {
    pub theme: Option<String>,
    /// Lay the colours of the session's Regolith look over the theme (see
    /// [`look`]); `overrides` still take precedence.
    pub regolith_look: bool,
    pub overrides: Option<ThemeOverrides>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct ThemeOverrides {
    pub idle_bg: Option<ColorOrLink>,
    pub idle_fg: Option<ColorOrLink>,
    pub info_bg: Option<ColorOrLink>,
    pub info_fg: Option<ColorOrLink>,
    pub good_bg: Option<ColorOrLink>,
    pub good_fg: Option<ColorOrLink>,
    pub warning_bg: Option<ColorOrLink>,
    pub warning_fg: Option<ColorOrLink>,
    pub critical_bg: Option<ColorOrLink>,
    pub critical_fg: Option<ColorOrLink>,
    pub error_fg: Option<ColorOrLink>,
    pub icon_fg: Option<ColorOrLink>,
    pub separator: Option<Separator>,
    pub separator_bg: Option<ColorOrLink>,
    pub separator_fg: Option<ColorOrLink>,
    pub alternating_tint_bg: Option<ColorOrLink>,
    pub alternating_tint_fg: Option<ColorOrLink>,
    pub end_separator: Option<Separator>,
    pub start_separator: Option<Separator>,
}

impl TryFrom<ThemeUserConfig> for Theme {
    type Error = Error;

    fn try_from(user_config: ThemeUserConfig) -> Result<Self, Self::Error> {
        let name = user_config.theme.as_deref().unwrap_or("plain");
        let file = util::find_file(name, Some("themes"), Some("toml"))?
            .or_error(|| format!("Theme '{name}' not found"))?;
        let theme: ThemeInner = util::deserialize_toml_file(file)?;
        let mut theme = Theme(theme);
        if user_config.regolith_look
            && let Some(look) = look::session()
        {
            theme.apply_look(look);
        }
        if let Some(overrides) = user_config.overrides {
            theme.apply_overrides(overrides)?;
        }
        Ok(theme)
    }
}

#[derive(Debug, Clone)]
pub enum ColorOrLink {
    Color(Color),
    Link { link: String },
}

impl ColorOrLink {
    fn eval(self, theme: &Theme) -> Result<Color> {
        Ok(match self {
            Self::Color(c) => c,
            Self::Link { link } => match link.as_str() {
                "idle_bg" => theme.idle_bg,
                "idle_fg" => theme.idle_fg,
                "info_bg" => theme.info_bg,
                "info_fg" => theme.info_fg,
                "good_bg" => theme.good_bg,
                "good_fg" => theme.good_fg,
                "warning_bg" => theme.warning_bg,
                "warning_fg" => theme.warning_fg,
                "critical_bg" => theme.critical_bg,
                "critical_fg" => theme.critical_fg,
                "error_fg" => theme.error_fg(),
                "icon_fg" => theme.icon_fg,
                "separator_bg" => theme.separator_bg,
                "separator_fg" => theme.separator_fg,
                "alternating_tint_bg" => theme.alternating_tint_bg,
                "alternating_tint_fg" => theme.alternating_tint_fg,
                _ => return Err(Error::new(format!("{link} is not a correct theme color"))),
            },
        })
    }
}

impl<'de> Deserialize<'de> for ColorOrLink {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        struct Visitor;
        impl<'de> de::Visitor<'de> for Visitor {
            type Value = ColorOrLink;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("color or link")
            }

            fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
            where
                A: de::MapAccess<'de>,
            {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Link {
                    link: String,
                }
                Link::deserialize(de::value::MapAccessDeserializer::new(map))
                    .map(|link| ColorOrLink::Link { link: link.link })
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                v.parse::<Color>().serde_error().map(ColorOrLink::Color)
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::themes::color::Rgba;

    fn rgba(hex: u32) -> Color {
        Color::Rgba(Rgba::from_hex(hex))
    }

    fn semi_native() -> Theme {
        Theme(ThemeInner {
            idle_fg: rgba(0x93a1a1ff),
            info_fg: rgba(0x93a1a1ff),
            good_fg: rgba(0x859900ff),
            warning_fg: rgba(0xb58900ff),
            critical_fg: rgba(0xdc322fff),
            ..ThemeInner::default()
        })
    }

    fn overrides(entries: &str) -> ThemeOverrides {
        toml::from_str(entries).unwrap()
    }

    #[test]
    fn look_colours_replace_the_theme_colours() {
        let mut theme = semi_native();
        theme.apply_look(&Look::new(map! {
            look::LABEL_COLOR => "#abc",
            look::VALUE_COLOR => "#012345",
            look::NOMINAL_COLOR => "#234567",
            look::WARNING_COLOR => "#345678",
            look::CRITICAL_COLOR => "#456789",
            look::ERROR_COLOR => "#56789a",
        }));
        assert_eq!(theme.icon_fg, rgba(0xaabbccff));
        assert_eq!(theme.get_colors(State::Idle).1, rgba(0x012345ff));
        assert_eq!(theme.get_colors(State::Info).1, rgba(0x012345ff));
        assert_eq!(theme.get_colors(State::Good).1, rgba(0x234567ff));
        assert_eq!(theme.get_colors(State::Warning).1, rgba(0x345678ff));
        assert_eq!(theme.get_colors(State::Critical).1, rgba(0x456789ff));
        assert_eq!(theme.error_fg(), rgba(0x56789aff));
        assert_eq!(
            theme.look,
            Some(LookColors {
                battery_low_fg: rgba(0x56789aff)
            })
        );
        assert_eq!(theme.idle_bg, Color::None);
    }

    #[test]
    fn invalid_look_colours_keep_the_theme_colours() {
        let mut theme = semi_native();
        theme.apply_look(&Look::new(map! {
            look::LABEL_COLOR => "",
            look::VALUE_COLOR => "banana",
            look::WARNING_COLOR => "#345678",
            look::ERROR_COLOR => "#12GG00",
        }));
        assert_eq!(theme.icon_fg, Color::None);
        assert_eq!(theme.idle_fg, rgba(0x93a1a1ff));
        assert_eq!(theme.good_fg, rgba(0x859900ff));
        assert_eq!(theme.warning_fg, rgba(0x345678ff));
        assert_eq!(theme.error_fg(), rgba(0xdc322fff));
        // The battery's 21-50% band falls back to the theme's own warning
        // colour, not the look's.
        assert_eq!(theme.look.unwrap().battery_low_fg, rgba(0xb58900ff));

        // Errors keep the theme's colour, not the look's critical colour.
        let mut theme = semi_native();
        theme.apply_look(&Look::new(map! { look::CRITICAL_COLOR => "#456789" }));
        assert_eq!(theme.critical_fg, rgba(0x456789ff));
        assert_eq!(theme.error_fg(), rgba(0xdc322fff));
    }

    #[test]
    fn errors_follow_an_overridden_critical_colour() {
        let mut theme = semi_native();
        theme.apply_look(&Look::new(map! { look::ERROR_COLOR => "#56789a" }));
        theme
            .apply_overrides(overrides(r##"critical_fg = "#abcdef""##))
            .unwrap();
        assert_eq!(theme.error_fg(), rgba(0xabcdefff));

        let mut theme = semi_native();
        theme
            .apply_overrides(overrides(
                r##"
                critical_fg = "#abcdef"
                error_fg = "#fedcba"
                "##,
            ))
            .unwrap();
        assert_eq!(theme.error_fg(), rgba(0xfedcbaff));
        assert_eq!(theme.critical_fg, rgba(0xabcdefff));
    }

    #[test]
    fn icon_colour_override() {
        let mut theme = semi_native();
        theme.apply_look(&Look::new(map! { look::LABEL_COLOR => "#abc" }));
        theme
            .apply_overrides(overrides(r#"icon_fg = "none""#))
            .unwrap();
        assert_eq!(theme.icon_fg, Color::None);
        theme
            .apply_overrides(overrides(r#"icon_fg = { link = "good_fg" }"#))
            .unwrap();
        assert_eq!(theme.icon_fg, rgba(0x859900ff));
    }
}
