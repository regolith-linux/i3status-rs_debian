//! The session's Regolith look
//!
//! Regolith keeps the colours and symbols of the active look as session
//! resources, served on the session bus by `trawld`: the values
//! `trawlcat <name>` prints, including the user's own overrides that
//! `regolith-look` folds in from `~/.config/regolith3/Xresources`. The X11
//! bar draws its blocks from the `i3xrocks.*` resources among them.
//!
//! With `regolith_look = true` in the `[theme]` or `[icons]` table, the bar
//! reads these resources once each time it starts (sway starts it again on
//! every reload, which `regolith-look set` and `regolith-look refresh`
//! trigger, and an in-place restart with `SIGUSR2` is a start as well) and
//! lays them over the configured theme or icon set; see
//! [`Theme::apply_look`](crate::themes::Theme::apply_look) and
//! [`Icons::apply_look`](crate::icons::Icons::apply_look). A resource that is
//! missing, empty or invalid leaves that one colour or symbol as the theme or
//! icon set has it. Without a session bus, or without the resource service on
//! it, nothing is applied: the bar never waits longer than [`FETCH_TIMEOUT`]
//! for an answer.

use std::collections::HashMap;
use std::sync::{LazyLock, mpsc};
use std::time::Duration;

use crate::escape::Escaped as _;
use crate::themes::color::{Color, Rgba};

make_log_macro!(debug, "look");

/// How long a start may wait for the resource service.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(2);

/// Colour of a block's symbol.
pub const LABEL_COLOR: &str = "i3xrocks.label.color";
/// Colour of a reading at rest or for information.
pub const VALUE_COLOR: &str = "i3xrocks.value.color";
/// Colour of a good reading.
pub const NOMINAL_COLOR: &str = "i3xrocks.nominal";
/// Colour of a warning.
pub const WARNING_COLOR: &str = "i3xrocks.warning";
/// Colour of a critical reading.
pub const CRITICAL_COLOR: &str = "i3xrocks.critical.color";
/// Colour of an error message (and of a battery between 21% and 50%).
pub const ERROR_COLOR: &str = "i3xrocks.error.color";

pub const CPU_SYMBOL: &str = "i3xrocks.label.cpu";
pub const MEMORY_SYMBOL: &str = "i3xrocks.label.memory";
pub const DISK_SYMBOL: &str = "i3xrocks.label.disk";
pub const SOUND_SYMBOL: &str = "i3xrocks.label.sound";
pub const MUTE_SYMBOL: &str = "i3xrocks.label.mute";
pub const TIME_SYMBOL: &str = "i3xrocks.label.time";
/// Symbol of a charging battery, shown after its percentage.
pub const CHARGING_SYMBOL: &str = "i3xrocks.label.up";
/// Symbol of a discharging battery, shown after its percentage.
pub const DISCHARGING_SYMBOL: &str = "i3xrocks.label.dn";

/// The battery symbols by charge, as the X11 bar grades them: each applies
/// up to and including its percentage.
pub const BATTERY_SYMBOLS: [(u8, &str); 5] = [
    (20, "i3xrocks.label.battery0"),
    (50, "i3xrocks.label.battery20"),
    (80, "i3xrocks.label.battery50"),
    (98, "i3xrocks.label.battery80"),
    (100, "i3xrocks.label.battery100"),
];

/// The index of the battery band (see [`BATTERY_SYMBOLS`]) a shown
/// percentage falls in.
pub fn battery_band(percentage: f64) -> usize {
    let percentage = percentage.round();
    BATTERY_SYMBOLS
        .iter()
        .position(|&(up_to, _)| percentage <= up_to as f64)
        .unwrap_or(BATTERY_SYMBOLS.len() - 1)
}

/// A set of session resources.
#[derive(Debug, Clone, Default)]
pub struct Look {
    resources: HashMap<String, String>,
}

impl Look {
    pub fn new(resources: HashMap<String, String>) -> Self {
        Self { resources }
    }

    /// The value of a resource, if it is set to something other than blank
    /// space. As `xrescat` prints it, surrounding blank space and one pair of
    /// surrounding double quotes are removed.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.resources
            .get(name)
            .map(|value| {
                let value = value.trim();
                value
                    .strip_prefix('"')
                    .and_then(|inner| inner.strip_suffix('"'))
                    .unwrap_or(value)
            })
            .filter(|value| !value.trim().is_empty())
    }

    /// A colour resource, if it holds a valid colour (see [`parse_color`]).
    pub fn color(&self, name: &str) -> Option<Color> {
        self.get(name).and_then(parse_color)
    }

    /// A symbol resource as Pango markup: the resource's text is shown
    /// literally.
    pub fn symbol(&self, name: &str) -> Option<String> {
        self.get(name).map(|text| text.pango_escaped())
    }
}

/// Parse a colour as the looks write them: `#RGB`, `#RRGGBB` or `#RRGGBBAA`,
/// in either case, with surrounding blank space ignored. `#abc` is
/// `#AABBCC`.
pub fn parse_color(text: &str) -> Option<Color> {
    let digits = text.trim().strip_prefix('#')?;
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let rgba = match digits.len() {
        3 => digits.chars().flat_map(|c| [c, c]).collect::<String>() + "ff",
        6 => format!("{digits}ff"),
        8 => digits.to_string(),
        _ => return None,
    };
    u32::from_str_radix(&rgba, 16)
        .ok()
        .map(|hex| Color::Rgba(Rgba::from_hex(hex)))
}

#[zbus::proxy(
    interface = "org.regolith.trawl1",
    default_service = "org.regolith.Trawl",
    default_path = "/org/regolith/Trawl"
)]
trait Trawl {
    #[zbus(property)]
    fn resources(&self) -> zbus::Result<HashMap<String, String>>;
}

static SESSION: LazyLock<Option<Look>> = LazyLock::new(|| fetch().map(Look::new));

/// The session's resources, read the first time they are asked for in this
/// run of the bar. `None` without a session bus or a resource service.
pub fn session() -> Option<&'static Look> {
    SESSION.as_ref()
}

fn fetch() -> Option<HashMap<String, String>> {
    // Configuration is parsed from within the bar's runtime, which cannot
    // block on another future; the query gets a thread and a runtime of its
    // own, and is abandoned if it does not answer in time.
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("regolith-look".into())
        .spawn(move || {
            let resources = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|err| err.to_string())
                .and_then(|runtime| {
                    runtime
                        .block_on(async {
                            let connection = zbus::Connection::session().await?;
                            TrawlProxy::builder(&connection)
                                .cache_properties(zbus::proxy::CacheProperties::No)
                                .build()
                                .await?
                                .resources()
                                .await
                        })
                        .map_err(|err| err.to_string())
                });
            let _ = sender.send(resources);
        })
        .ok()?;
    match receiver.recv_timeout(FETCH_TIMEOUT) {
        Ok(Ok(resources)) => Some(resources),
        Ok(Err(err)) => {
            debug!("session resources unavailable: {err}");
            None
        }
        Err(_) => {
            debug!("no answer from the resource service in {FETCH_TIMEOUT:?}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(hex: u32) -> Color {
        Color::Rgba(Rgba::from_hex(hex))
    }

    #[test]
    fn colour_forms() {
        assert_eq!(parse_color("#abc"), Some(rgba(0xaabbccff)));
        assert_eq!(parse_color("#ABC"), Some(rgba(0xaabbccff)));
        assert_eq!(parse_color("#A1b2C3"), Some(rgba(0xa1b2c3ff)));
        assert_eq!(parse_color("#a1b2c3cc"), Some(rgba(0xa1b2c3cc)));
        assert_eq!(parse_color("  #DEF123 \t"), Some(rgba(0xdef123ff)));
    }

    #[test]
    fn invalid_colours() {
        for text in [
            "",
            " ",
            "banana",
            "red",
            "123456",
            "#",
            "#abcd",
            "#12345",
            "#1234567",
            "#123456789",
            "#12GG00",
            "# abc",
            "#+12345",
        ] {
            assert_eq!(parse_color(text), None, "{text:?}");
        }
    }

    #[test]
    fn blank_resources_are_unset() {
        let look = Look::new(map! {
            "empty" => "",
            "blank" => "  ",
            "set" => " <&x> ",
        });
        assert_eq!(look.get("missing"), None);
        assert_eq!(look.get("empty"), None);
        assert_eq!(look.get("blank"), None);
        assert_eq!(look.symbol("set").as_deref(), Some("&lt;&amp;x&gt;"));
    }

    #[test]
    fn quotes_are_removed_like_xrescat() {
        let look = Look::new(map! {
            "quoted" => " \"#abc\" ",
            "nested" => "\"\"x\"\"",
            "inner" => "a\"b",
            "lone" => "\"",
            "open" => "\"x",
            "empty" => "\"\"",
            "blank" => "\"  \"",
        });
        assert_eq!(look.color("quoted"), Some(rgba(0xaabbccff)));
        assert_eq!(look.get("nested"), Some("\"x\""));
        assert_eq!(look.get("inner"), Some("a\"b"));
        assert_eq!(look.get("lone"), Some("\""));
        assert_eq!(look.get("open"), Some("\"x"));
        assert_eq!(look.get("empty"), None);
        assert_eq!(look.get("blank"), None);
    }

    #[test]
    fn battery_bands() {
        for (percentage, band) in [
            (0.0, 0),
            (20.0, 0),
            (20.4, 0),
            (20.5, 1),
            (21.0, 1),
            (50.0, 1),
            (51.0, 2),
            (80.0, 2),
            (81.0, 3),
            (98.0, 3),
            (99.0, 4),
            (100.0, 4),
        ] {
            assert_eq!(battery_band(percentage), band, "{percentage}");
        }
    }
}
