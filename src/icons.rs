use crate::errors::*;
use crate::look::{self, Look};
use crate::util;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Deserialize, Debug, Clone)]
#[serde(try_from = "IconsConfigRaw")]
pub struct Icons(pub HashMap<String, Icon>);

#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum Icon {
    Single(String),
    Progression(Vec<String>),
}

impl From<&'static str> for Icon {
    fn from(value: &'static str) -> Self {
        Self::Single(value.into())
    }
}

impl<const N: usize> From<[&str; N]> for Icon {
    fn from(value: [&str; N]) -> Self {
        Self::Progression(value.iter().map(|s| s.to_string()).collect())
    }
}

/// Every icon key the bar knows. Blocks name icons through these consts,
/// and `ALL` lets the tests check the default set covers every key.
macro_rules! icon_keys {
    ($($name:ident = $key:literal,)*) => {
        $(pub(crate) const $name: &str = $key;)*

        #[cfg(test)]
        const ALL: &[&str] = &[$($name),*];
    };
}

icon_keys! {
    BACKLIGHT = "backlight",
    BAT = "bat",
    BAT_CHARGING = "bat_charging",
    BAT_DISCHARGING = "bat_discharging",
    BAT_NOT_AVAILABLE = "bat_not_available",
    BELL = "bell",
    BELL_SLASH = "bell-slash",
    BLUETOOTH = "bluetooth",
    CALENDAR = "calendar",
    COGS = "cogs",
    CPU = "cpu",
    CPU_BOOST_OFF = "cpu_boost_off",
    CPU_BOOST_ON = "cpu_boost_on",
    DISK_DRIVE = "disk_drive",
    DOCKER = "docker",
    GITHUB = "github",
    GPU = "gpu",
    HEADPHONES = "headphones",
    HUESHIFT = "hueshift",
    JOYSTICK = "joystick",
    KEYBOARD = "keyboard",
    MAIL = "mail",
    MEMORY_MEM = "memory_mem",
    MEMORY_SWAP = "memory_swap",
    MICROPHONE = "microphone",
    MICROPHONE_MUTED = "microphone_muted",
    MOUSE = "mouse",
    MUSIC = "music",
    MUSIC_NEXT = "music_next",
    MUSIC_PAUSE = "music_pause",
    MUSIC_PLAY = "music_play",
    MUSIC_PREV = "music_prev",
    NET_BRIDGE = "net_bridge",
    NET_CELLULAR = "net_cellular",
    NET_DOWN = "net_down",
    NET_LOOPBACK = "net_loopback",
    NET_MODEM = "net_modem",
    NET_UP = "net_up",
    NET_VPN = "net_vpn",
    NET_WIRED = "net_wired",
    NET_WIRELESS = "net_wireless",
    NOTIFICATION = "notification",
    PHONE = "phone",
    PHONE_DISCONNECTED = "phone_disconnected",
    PING = "ping",
    POMODORO = "pomodoro",
    POMODORO_BREAK = "pomodoro_break",
    POMODORO_PAUSED = "pomodoro_paused",
    POMODORO_STARTED = "pomodoro_started",
    POMODORO_STOPPED = "pomodoro_stopped",
    REFRESH = "refresh",
    RESOLUTION = "resolution",
    SCRATCHPAD = "scratchpad",
    TASKS = "tasks",
    TEA = "tea",
    THERMOMETER = "thermometer",
    TIME = "time",
    TOGGLE_OFF = "toggle_off",
    TOGGLE_ON = "toggle_on",
    UNKNOWN = "unknown",
    UPDATE = "update",
    UPTIME = "uptime",
    VOLUME = "volume",
    VOLUME_MUTED = "volume_muted",
    WEATHER_CLOUDS = "weather_clouds",
    WEATHER_CLOUDS_NIGHT = "weather_clouds_night",
    WEATHER_DEFAULT = "weather_default",
    WEATHER_FOG = "weather_fog",
    WEATHER_FOG_NIGHT = "weather_fog_night",
    WEATHER_MOON = "weather_moon",
    WEATHER_RAIN = "weather_rain",
    WEATHER_RAIN_NIGHT = "weather_rain_night",
    WEATHER_SNOW = "weather_snow",
    WEATHER_SUN = "weather_sun",
    WEATHER_THUNDER = "weather_thunder",
    WEATHER_THUNDER_NIGHT = "weather_thunder_night",
    WEBCAM = "webcam",
    XRANDR = "xrandr",
}

impl Default for Icons {
    fn default() -> Self {
        // "none" icon set
        Self(map! {
            BACKLIGHT => "BRIGHT",
            BAT => "BAT",
            BAT_CHARGING => "CHG",
            BAT_DISCHARGING => "",
            BAT_NOT_AVAILABLE => "BAT N/A",
            BELL => "ON",
            BELL_SLASH => "OFF",
            BLUETOOTH => "BT",
            CALENDAR => "CAL",
            COGS => "LOAD",
            CPU => "CPU",
            CPU_BOOST_ON => "BOOST ON",
            CPU_BOOST_OFF => "BOOST OFF",
            DISK_DRIVE => "DISK",
            DOCKER => "DOCKER",
            GITHUB => "GITHUB",
            GPU => "GPU",
            HEADPHONES => "HEAD",
            HUESHIFT => "HUE",
            JOYSTICK => "JOY",
            KEYBOARD => "KBD",
            MAIL => "MAIL",
            MEMORY_MEM => "MEM",
            MEMORY_SWAP => "SWAP",
            MOUSE => "MOUSE",
            MUSIC => "MUSIC",
            MUSIC_NEXT => ">",
            MUSIC_PAUSE => "||",
            MUSIC_PLAY => ">",
            MUSIC_PREV => "<",
            NET_BRIDGE => "BRIDGE",
            NET_CELLULAR => [
                                "NO SIGNAL",
                                "0 BARS",
                                "1 BAR",
                                "2 BARS",
                                "3 BARS",
                                "4 BARS",
                              ],
            NET_DOWN => "DOWN",
            NET_LOOPBACK => "LO",
            NET_MODEM => "MODEM",
            NET_UP => "UP ",
            NET_VPN => "VPN",
            NET_WIRED => "ETH",
            NET_WIRELESS => "WLAN",
            NOTIFICATION => "NOTIF",
            PHONE => "PHONE",
            PHONE_DISCONNECTED => "PHONE",
            PING => "PING",
            POMODORO => "POMODORO",
            POMODORO_BREAK => "BREAK",
            POMODORO_PAUSED => "PAUSED",
            POMODORO_STARTED => "STARTED",
            POMODORO_STOPPED => "STOPPED",
            REFRESH => "REFRESH",
            RESOLUTION => "RES",
            SCRATCHPAD => "[]",
            TASKS => "TSK",
            TEA => "TEA",
            THERMOMETER => "TEMP",
            TIME => "TIME",
            TOGGLE_OFF => "OFF",
            TOGGLE_ON => "ON",
            UNKNOWN => "??",
            UPDATE => "UPD",
            UPTIME => "UP",
            VOLUME => "VOL",
            VOLUME_MUTED => "VOL MUTED",
            MICROPHONE => "MIC",
            MICROPHONE_MUTED => "MIC MUTED",
            WEATHER_CLOUDS_NIGHT => "CLOUDY",
            WEATHER_CLOUDS => "CLOUDY",
            WEATHER_DEFAULT => "WEATHER",
            WEATHER_FOG_NIGHT => "FOG",
            WEATHER_FOG => "FOG",
            WEATHER_MOON => "MOONY",
            WEATHER_RAIN_NIGHT => "RAIN",
            WEATHER_RAIN => "RAIN",
            WEATHER_SNOW => "SNOW",
            WEATHER_SUN => "SUNNY",
            WEATHER_THUNDER_NIGHT => "STORM",
            WEATHER_THUNDER => "STORM",
            WEBCAM => "CAM",
            XRANDR => "SCREEN"
        })
    }
}

impl Icons {
    pub fn from_file(file: &str) -> Result<Self> {
        if file == "none" {
            Ok(Icons::default())
        } else {
            let file = util::find_file(file, Some("icons"), Some("toml"))?
                .or_error(|| format!("Icon set '{file}' not found"))?;
            Ok(Icons(util::deserialize_toml_file(file)?))
        }
    }

    /// Take the symbols of a Regolith look (see [`look`]). Each symbol the
    /// look does not define, or defines as empty, stays as it is.
    ///
    /// `cpu` takes `i3xrocks.label.cpu`; `memory_mem` and `memory_swap`
    /// `i3xrocks.label.memory`; `disk_drive` `i3xrocks.label.disk`; `volume`
    /// `i3xrocks.label.sound` and `volume_muted` `i3xrocks.label.mute`;
    /// `time` `i3xrocks.label.time`; `bat_charging` and `bat_discharging`
    /// `i3xrocks.label.up` and `i3xrocks.label.dn`. `bat` follows the X11
    /// bar's grading: `i3xrocks.label.battery0` up to 20%, `battery20` up to
    /// 50%, `battery50` up to 80%, `battery80` up to 98% and `battery100`
    /// above, each band keeping the set's own icon if the look has none.
    pub fn apply_look(&mut self, look: &Look) {
        for (icon, resource) in [
            (CPU, look::CPU_SYMBOL),
            (MEMORY_MEM, look::MEMORY_SYMBOL),
            (MEMORY_SWAP, look::MEMORY_SYMBOL),
            (DISK_DRIVE, look::DISK_SYMBOL),
            (VOLUME, look::SOUND_SYMBOL),
            (VOLUME_MUTED, look::MUTE_SYMBOL),
            (TIME, look::TIME_SYMBOL),
            (BAT_CHARGING, look::CHARGING_SYMBOL),
            (BAT_DISCHARGING, look::DISCHARGING_SYMBOL),
        ] {
            if let Some(symbol) = look.symbol(resource) {
                self.0.insert(icon.into(), Icon::Single(symbol));
            }
        }

        // The bands are uneven, so `bat` becomes a progression with one
        // entry per whole percent (the battery block picks it by the
        // percentage it shows).
        let bands = look::BATTERY_SYMBOLS.map(|(_, resource)| look.symbol(resource));
        if bands.iter().any(Option::is_some) {
            let progression: Option<Vec<String>> = (0..=100u8)
                .map(|percent| match &bands[look::battery_band(percent.into())] {
                    Some(symbol) => Some(symbol.clone()),
                    None => self
                        .get(BAT, Some(f64::from(percent) / 100.0))
                        .map(str::to_string),
                })
                .collect();
            if let Some(progression) = progression {
                self.0.insert(BAT.into(), Icon::Progression(progression));
            }
        }
    }

    pub fn apply_overrides(&mut self, overrides: HashMap<String, Icon>) {
        self.0.extend(overrides);
    }

    pub fn get(&self, icon: &'_ str, value: Option<f64>) -> Option<&str> {
        match (self.0.get(icon)?, value) {
            (Icon::Single(icon), _) => Some(icon),
            (Icon::Progression(prog), _) if prog.is_empty() => None,
            (Icon::Progression(prog), None) => Some(prog.last().unwrap()),
            (Icon::Progression(prog), Some(value)) => {
                let index = ((value * prog.len() as f64) as usize).clamp(0, prog.len() - 1);
                Some(prog[index].as_str())
            }
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct IconsConfigRaw {
    icons: Option<String>,
    /// Lay the symbols of the session's Regolith look over the icon set (see
    /// [`look`]); `overrides` still take precedence.
    regolith_look: bool,
    overrides: Option<HashMap<String, Icon>>,
}

impl TryFrom<IconsConfigRaw> for Icons {
    type Error = Error;

    fn try_from(raw: IconsConfigRaw) -> Result<Self, Self::Error> {
        let mut icons = Self::from_file(raw.icons.as_deref().unwrap_or("none"))?;
        if raw.regolith_look
            && let Some(look) = look::session()
        {
            icons.apply_look(look);
        }
        if let Some(overrides) = raw.overrides {
            for icon in overrides {
                icons.0.insert(icon.0, icon.1);
            }
        }
        Ok(icons)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_icons_cover_every_key() {
        let icons = Icons::default();
        let mut defaults: Vec<&str> = icons.0.keys().map(String::as_str).collect();
        let mut keys = ALL.to_vec();
        defaults.sort_unstable();
        keys.sort_unstable();
        assert_eq!(defaults, keys);
    }
}

#[cfg(test)]
mod look_tests {
    use super::*;

    fn awesome4_like() -> Icons {
        Icons(map! {
            CPU => Icon::from("cpu"),
            VOLUME => Icon::from(["v0", "v1", "v2"]),
            BAT => Icon::from(["b0", "b1", "b2", "b3", "b4"]),
            BAT_CHARGING => Icon::from("plug"),
            BAT_DISCHARGING => Icon::from(""),
        })
    }

    #[test]
    fn look_symbols_replace_icons() {
        let mut icons = awesome4_like();
        icons.apply_look(&Look::new(map! {
            look::CPU_SYMBOL => "<&>",
            look::SOUND_SYMBOL => "s",
            look::MEMORY_SYMBOL => "m",
            look::DISCHARGING_SYMBOL => "dn",
            look::CHARGING_SYMBOL => "",
        }));
        assert_eq!(icons.get(CPU, None), Some("&lt;&amp;&gt;"));
        assert_eq!(icons.get(VOLUME, Some(0.0)), Some("s"));
        assert_eq!(icons.get(MEMORY_MEM, None), Some("m"));
        assert_eq!(icons.get(MEMORY_SWAP, None), Some("m"));
        assert_eq!(icons.get(BAT_DISCHARGING, None), Some("dn"));
        assert_eq!(icons.get(BAT_CHARGING, None), Some("plug"));
        // No battery symbols: the set's own progression stays.
        assert!(matches!(icons.0.get(BAT), Some(Icon::Progression(p)) if p.len() == 5));
    }

    #[test]
    fn battery_bands_with_fallbacks() {
        let mut icons = awesome4_like();
        icons.apply_look(&Look::new(map! {
            "i3xrocks.label.battery0" => "B0",
            "i3xrocks.label.battery20" => "B20",
            "i3xrocks.label.battery80" => "B80",
            "i3xrocks.label.battery100" => "B100",
        }));
        for (percent, symbol) in [
            (0, "B0"),
            (20, "B0"),
            (21, "B20"),
            (50, "B20"),
            // battery50 is missing: the set's own icon for the percentage
            (51, "b2"),
            (60, "b3"),
            (80, "b4"),
            (81, "B80"),
            (98, "B80"),
            (99, "B100"),
            (100, "B100"),
        ] {
            assert_eq!(
                icons.get(BAT, Some(f64::from(percent) / 100.0)),
                Some(symbol),
                "{percent}%"
            );
        }
    }
}
