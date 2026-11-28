//! Monitor Bluetooth devices
//!
//! This block has two modes, selected by whether `mac` is set.
//!
//! With `mac`, it displays the connectivity of the given Bluetooth device and the battery level
//! if this is supported. When the device can be identified as an audio headset, a keyboard,
//! joystick, or mouse, use the relevant icon. Otherwise, fall back on the generic Bluetooth
//! symbol. Right-clicking the block will attempt to connect (or disconnect) the device.
//!
//! Without `mac`, it lists every Bluetooth device that is currently connected (on the adapter
//! selected by `adapter_mac`, or on every adapter). Each device is shown by its alias, falling back
//! on its name and then on its address. The block is hidden while no device is connected, unless
//! `disconnected_format` is set. The list follows BlueZ's change notifications (devices, aliases,
//! adapters and the service itself appearing or going away), so the block does not poll.
//!
//! Relies on the Bluez D-Bus API.
//!
//! Note: battery level information is not reported for some devices. [Enabling experimental
//! features of `bluez`](https://wiki.archlinux.org/title/bluetooth#Enabling_experimental_features)
//! may fix it.
//!
//! # Configuration
//!
//! Key | Values | Default
//! ----|--------|--------
//! `mac` | MAC address of the Bluetooth device. If not set, every connected device is listed. | `None`
//! `adapter_mac` | MAC Address of the Bluetooth adapter (in case your device was connected to multiple currently available adapters). Without `mac`, only devices on this adapter are listed. | `None`
//! `format` | A string to customise the output of this block. See below for available placeholders. | <code>\" $icon $name{ $percentage\|} \"</code>, or <code>\" $icon $names \"</code> without `mac`
//! `disconnected_format` | A string to customise the output of this block. See below for available placeholders. | <code>\" $icon{ $name\|} \"</code>, or hidden without `mac`
//! `battery_state` | A mapping from battery percentage to block's [state](State) (color). See example below. Only used with `mac`. | 0..15 -> critical, 16..30 -> warning, 31..60 -> info, 61..100 -> good
//!
//! Placeholders with `mac`:
//!
//! Placeholder    | Value                                                                 | Type   | Unit
//! ---------------|-----------------------------------------------------------------------|--------|------
//! `icon`         | Icon based on what type of device is connected                        | Icon   | -
//! `name`         | Device's name                                                         | Text   | -
//! `percentage`   | Device's battery level (may be absent if the device is not supported) | Number | %
//! `battery_icon` | Battery icon (may be absent if the device is not supported)           | Icon   | -
//! `available`    | Present if the device is available                                    | Flag   | -
//!
//! Placeholders without `mac`:
//!
//! Placeholder | Value                                                                                        | Type | Unit
//! ------------|----------------------------------------------------------------------------------------------|------|------
//! `icon`      | The Bluetooth icon                                                                           | Icon | -
//! `names`     | Names of the connected devices, sorted by Unicode case folding (then by address) and separated by `, ` (empty in `disconnected_format`) | Text | -
//! `count`     | Number of connected devices (`0` in `disconnected_format`)                                  | Text | -
//!
//! Action   | Default button
//! ---------|---------------
//! `toggle` | Right (only with `mac`)
//!
//! # Examples
//!
//! This example just shows the icon when device is connected.
//!
//! ```toml
//! [[block]]
//! block = "bluetooth"
//! mac = "00:18:09:92:1B:BA"
//! disconnected_format = ""
//! format = " $icon "
//! [block.battery_state]
//! "0..20" = "critical"
//! "21..70" = "warning"
//! "71..100" = "good"
//! ```
//!
//! This example lists every connected device, and is hidden when there is none.
//!
//! ```toml
//! [[block]]
//! block = "bluetooth"
//! ```
//!
//! # Icons Used
//! - `headphones` (`$icon`) for bluetooth devices identifying as "audio-card", "audio-headset" or "audio-headphones"
//! - `joystick` (`$icon`) for bluetooth devices identifying as "input-gaming"
//! - `keyboard` (`$icon`) for bluetooth devices identifying as "input-keyboard"
//! - `mouse` (`$icon`) for bluetooth devices identifying as "input-mouse"
//! - `bluetooth` (`$icon`) for all other devices, when the device is unavailable, and without `mac`
//! - `bat` (`$battery_icon`, as a progression) for devices reporting a battery level

use std::num::NonZeroU32;

use zbus::fdo::{DBusProxy, ObjectManagerProxy, PropertiesProxy};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue};
use zbus::{MatchRule, MessageStream};

use super::prelude::*;
use crate::wrappers::RangeMap;

make_log_macro!(debug, "bluetooth");

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub mac: Option<String>,
    #[serde(default)]
    pub adapter_mac: Option<String>,
    #[serde(default)]
    pub format: FormatConfig,
    #[serde(default)]
    pub disconnected_format: FormatConfig,
    #[serde(default)]
    pub battery_state: Option<RangeMap<u8, State>>,
}

/// Every icon name [`device_icon`] can return. The device type is externally
/// selected but finite, so the block plan declares the full set.
const DEVICE_ICONS: [&str; 5] = [
    icons::HEADPHONES,
    icons::JOYSTICK,
    icons::KEYBOARD,
    icons::MOUSE,
    icons::BLUETOOTH,
];

fn device_icon(bluez_icon: Option<&str>) -> &'static str {
    match bluez_icon {
        Some("audio-card" | "audio-headset" | "audio-headphones") => icons::HEADPHONES,
        Some("input-gaming") => icons::JOYSTICK,
        Some("input-keyboard") => icons::KEYBOARD,
        Some("input-mouse") => icons::MOUSE,
        _ => icons::BLUETOOTH,
    }
}

pub(crate) fn prepare(config: &Config) -> Result<Arc<BlockPlan>> {
    if config.mac.is_none() {
        return prepare_all_devices(config);
    }
    let format = config.format.with_default(" $icon $name{ $percentage|} ")?;
    let disconnected_format = config
        .disconnected_format
        .with_default(" $icon{ $name|} ")?;
    BlockPlan::new(vec![
        OutputPlan::new("connected", format)
            .icon("icon", IconChoices::fixed(DEVICE_ICONS))
            .icon("battery_icon", IconChoices::one(icons::BAT)),
        OutputPlan::new("disconnected", disconnected_format.clone())
            .icon("icon", IconChoices::fixed(DEVICE_ICONS))
            .icon("battery_icon", IconChoices::one(icons::BAT)),
        OutputPlan::new("unavailable", disconnected_format)
            .icon("icon", IconChoices::one(icons::BLUETOOTH)),
    ])
}

/// Without `mac`: one output listing the connected devices, and one for when there is none. The
/// latter is only rendered if `disconnected_format` is configured; otherwise the block hides.
fn prepare_all_devices(config: &Config) -> Result<Arc<BlockPlan>> {
    let format = config.format.with_default(" $icon $names ")?;
    let disconnected_format = config.disconnected_format.with_default("")?;
    BlockPlan::new(vec![
        OutputPlan::new("devices", format).icon("icon", IconChoices::one(icons::BLUETOOTH)),
        OutputPlan::new("no_devices", disconnected_format)
            .icon("icon", IconChoices::one(icons::BLUETOOTH)),
    ])
}

pub(crate) async fn run(config: &Config, api: &CommonApi, plan: &Arc<BlockPlan>) -> Result<()> {
    let Some(mac) = &config.mac else {
        return run_all_devices(config, api, plan).await;
    };

    let mut actions = api.get_actions()?;
    api.set_default_actions(&[(MouseButton::Right, None, "toggle")])?;

    let output_connected = plan.output("connected")?;
    let output_disconnected = plan.output("disconnected")?;
    let output_unavailable = plan.output("unavailable")?;

    let mut monitor = DeviceMonitor::new(mac.clone(), config.adapter_mac.clone()).await?;

    let battery_states = config.battery_state.clone().unwrap_or_else(|| {
        vec![
            (0..=15, State::Critical),
            (16..=30, State::Warning),
            (31..=60, State::Info),
            (61..=100, State::Good),
        ]
        .into()
    });

    loop {
        match monitor.get_device_info().await {
            // Available
            Some(device) => {
                debug!("Device available, info: {device:?}");

                let output = if device.connected {
                    &output_connected
                } else {
                    &output_disconnected
                };
                let mut widget = output.new_widget();

                let values = map! {
                    "icon" => Value::icon(device.icon),
                    "name" => Value::text(device.name),
                    "available" => Value::flag(),
                    [if let Some(p) = device.battery_percentage] "percentage" => Value::percents(p),
                    [if let Some(p) = device.battery_percentage]
                        "battery_icon" => Value::icon_progression(icons::BAT, p as f64 / 100.0),
                };

                if device.connected {
                    widget.state = battery_states
                        .get(&device.battery_percentage.unwrap_or(100))
                        .copied()
                        .unwrap_or(State::Good);
                } else {
                    widget.state = State::Idle;
                }

                widget.set_values(values);
                api.set_widget(widget)?;
            }
            // Unavailable
            None => {
                debug!("Showing device as unavailable");
                let mut widget = output_unavailable.new_widget();
                widget.set_values(map!("icon" => Value::icon(icons::BLUETOOTH)));
                api.set_widget(widget)?;
            }
        }

        loop {
            select! {
                res = monitor.wait_for_change() => {
                    res?;
                    break;
                },
                Some(action) = actions.recv() => match action.as_ref() {
                    "toggle" => {
                        if let Some(dev) = &monitor.device
                            && let Ok(connected) = dev.device.connected().await {
                                if connected {
                                    let _ = dev.device.disconnect().await;
                                } else {
                                    let _ = dev.device.connect().await;
                                }
                                break;
                            }
                    }
                    _ => (),
                }
            }
        }
    }
}

struct DeviceMonitor {
    mac: String,
    adapter_mac: Option<String>,
    manager_proxy: ObjectManagerProxy<'static>,
    device: Option<Device>,
}

struct Device {
    props: PropertiesProxy<'static>,
    device: Device1Proxy<'static>,
    battery: Battery1Proxy<'static>,
}

#[derive(Debug)]
struct DeviceInfo {
    connected: bool,
    icon: &'static str,
    name: String,
    battery_percentage: Option<u8>,
}

impl DeviceMonitor {
    async fn new(mac: String, adapter_mac: Option<String>) -> Result<Self> {
        let dbus_conn = new_system_dbus_connection().await?;
        let manager_proxy = ObjectManagerProxy::builder(&dbus_conn)
            .destination("org.bluez")
            .and_then(|x| x.path("/"))
            .unwrap()
            .build()
            .await
            .error("Failed to create ObjectManagerProxy")?;
        let device = Device::try_find(&manager_proxy, &mac, adapter_mac.as_deref()).await?;
        Ok(Self {
            mac,
            adapter_mac,
            manager_proxy,
            device,
        })
    }

    async fn wait_for_change(&mut self) -> Result<()> {
        match &mut self.device {
            None => {
                let mut interface_added = self
                    .manager_proxy
                    .receive_interfaces_added()
                    .await
                    .error("Failed to monitor interfaces")?;
                loop {
                    interface_added
                        .next()
                        .await
                        .error("Stream ended unexpectedly")?;
                    if let Some(device) = Device::try_find(
                        &self.manager_proxy,
                        &self.mac,
                        self.adapter_mac.as_deref(),
                    )
                    .await?
                    {
                        self.device = Some(device);
                        debug!("Device has been added");
                        return Ok(());
                    }
                }
            }
            Some(device) => {
                let mut updates = device
                    .props
                    .receive_properties_changed()
                    .await
                    .error("Failed to receive updates")?;

                let mut interface_added = self
                    .manager_proxy
                    .receive_interfaces_added()
                    .await
                    .error("Failed to monitor interfaces")?;

                let mut interface_removed = self
                    .manager_proxy
                    .receive_interfaces_removed()
                    .await
                    .error("Failed to monitor interfaces")?;

                let mut bluez_owner_changed =
                    DBusProxy::new(self.manager_proxy.inner().connection())
                        .await
                        .error("Failed to create DBusProxy")?
                        .receive_name_owner_changed_with_args(&[(0, "org.bluez")])
                        .await
                        .unwrap();

                loop {
                    select! {
                        _ = updates.next_debounced() => {
                            debug!("Got update for device");
                            return Ok(());
                        }
                        Some(event) = interface_added.next() => {
                            let args = event.args().error("Failed to get the args")?;
                            if args.object_path() == device.device.inner().path() {
                                debug!("Interfaces added: {:?}", args.interfaces_and_properties().keys());
                                return Ok(());
                            }
                        }
                        Some(event) = interface_removed.next() => {
                            let args = event.args().error("Failed to get the args")?;
                            if args.object_path() == device.device.inner().path() {
                                self.device = None;
                                debug!("Device is no longer available");
                                return Ok(());
                            }
                        }
                        Some(event) = bluez_owner_changed.next() => {
                            let args = event.args().error("Failed to get the args")?;
                            if args.new_owner.is_none() {
                                self.device = None;
                                debug!("org.bluez disappeared");
                                return Ok(());
                            }
                        }
                    }
                }
            }
        }
    }

    async fn get_device_info(&mut self) -> Option<DeviceInfo> {
        let device = self.device.as_ref()?;

        let Ok((connected, name)) =
            tokio::try_join!(device.device.connected(), device.device.name(),)
        else {
            debug!("failed to fetch device info, assuming device or bluez disappeared");
            self.device = None;
            return None;
        };

        //icon can be null, so ignore errors when fetching it
        let icon: &str = device_icon(device.device.icon().await.ok().as_deref());

        Some(DeviceInfo {
            connected,
            icon,
            name,
            battery_percentage: device.battery.percentage().await.ok(),
        })
    }
}

impl Device {
    async fn try_find(
        manager_proxy: &ObjectManagerProxy<'_>,
        mac: &str,
        adapter_mac: Option<&str>,
    ) -> Result<Option<Self>> {
        let Ok(devices) = manager_proxy.get_managed_objects().await else {
            debug!("could not get the list of managed objects");
            return Ok(None);
        };

        debug!("all managed devices: {:?}", devices);

        let root_object: Option<String> = match adapter_mac {
            Some(adapter_mac) => {
                let mut adapter_path = None;
                for (path, interfaces) in &devices {
                    let adapter_interface = match interfaces.get("org.bluez.Adapter1") {
                        Some(i) => i,
                        None => continue, // Not an adapter
                    };
                    let addr: &str = adapter_interface
                        .get("Address")
                        .and_then(|a| a.downcast_ref().ok())
                        .unwrap();
                    if addr == adapter_mac {
                        adapter_path = Some(path);
                        break;
                    }
                }
                match adapter_path {
                    Some(path) => Some(format!("{}/", path.as_str())),
                    None => return Ok(None),
                }
            }
            None => None,
        };

        debug!("root object: {:?}", root_object);

        for (path, interfaces) in devices {
            if let Some(root) = &root_object
                && !path.starts_with(root)
            {
                continue;
            }

            let Some(device_interface) = interfaces.get("org.bluez.Device1") else {
                // Not a device
                continue;
            };

            let addr: &str = device_interface
                .get("Address")
                .and_then(|a| a.downcast_ref().ok())
                .unwrap();
            if addr != mac {
                continue;
            }

            debug!("Found device with path {:?}", path);

            return Ok(Some(Self {
                props: PropertiesProxy::builder(manager_proxy.inner().connection())
                    .destination("org.bluez")
                    .and_then(|x| x.path(path.clone()))
                    .unwrap()
                    .build()
                    .await
                    .error("Failed to create PropertiesProxy")?,
                device: Device1Proxy::builder(manager_proxy.inner().connection())
                    // No caching because https://github.com/greshake/i3status-rust/issues/1565#issuecomment-1379308681
                    .cache_properties(zbus::proxy::CacheProperties::No)
                    .path(path.clone())
                    .unwrap()
                    .build()
                    .await
                    .error("Failed to create Device1Proxy")?,
                battery: Battery1Proxy::builder(manager_proxy.inner().connection())
                    .cache_properties(zbus::proxy::CacheProperties::No)
                    .path(path)
                    .unwrap()
                    .build()
                    .await
                    .error("Failed to create Battery1Proxy")?,
            }));
        }

        debug!("No device found");
        Ok(None)
    }
}

async fn run_all_devices(config: &Config, api: &CommonApi, plan: &Arc<BlockPlan>) -> Result<()> {
    let output_devices = plan.output("devices")?;
    let output_no_devices = plan.output("no_devices")?;
    // Without a configured `disconnected_format`, the block hides while nothing is connected.
    let show_no_devices = config.disconnected_format.full.is_some();

    let mut monitor = BluezObjects::new(config.adapter_mac.clone()).await?;
    let mut shown: Option<Vec<ConnectedDevice>> = None;

    loop {
        let devices = monitor
            .state
            .connected_devices(config.adapter_mac.as_deref());
        if shown.as_ref() != Some(&devices) {
            debug!("connected devices: {devices:?}");
            if devices.is_empty() && !show_no_devices {
                api.hide()?;
            } else {
                let output = if devices.is_empty() {
                    &output_no_devices
                } else {
                    &output_devices
                };
                let names = devices
                    .iter()
                    .map(|d| d.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut widget = output.new_widget();
                widget.set_values(map! {
                    "icon" => Value::icon(icons::BLUETOOTH),
                    "names" => Value::text(names),
                    "count" => Value::text(devices.len().to_string()),
                });
                api.set_widget(widget)?;
            }
            shown = Some(devices);
        }

        monitor.wait_for_change().await?;
    }
}

const BLUEZ_SERVICE: &str = "org.bluez";
const ADAPTER_INTERFACE: &str = "org.bluez.Adapter1";
const DEVICE_INTERFACE: &str = "org.bluez.Device1";
const OBJECT_MANAGER_INTERFACE: &str = "org.freedesktop.DBus.ObjectManager";
const PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";

type Properties = HashMap<String, OwnedValue>;
type Interfaces = HashMap<String, Properties>;

fn str_property(props: &Properties, name: &str) -> Option<String> {
    props
        .get(name)
        .and_then(|v| v.downcast_ref::<&str>().ok())
        .map(Into::into)
}

fn bool_property(props: &Properties, name: &str) -> Option<bool> {
    props.get(name).and_then(|v| v.downcast_ref::<bool>().ok())
}

fn path_property(props: &Properties, name: &str) -> Option<String> {
    props
        .get(name)
        .and_then(|v| v.downcast_ref::<ObjectPath>().ok())
        .map(|p| p.as_str().into())
}

#[derive(Debug, Default)]
struct BluezAdapter {
    address: Option<String>,
    /// `None` until the service reports it.
    powered: Option<bool>,
}

impl BluezAdapter {
    fn update(&mut self, props: &Properties) {
        if let Some(address) = str_property(props, "Address") {
            self.address = Some(address);
        }
        if let Some(powered) = bool_property(props, "Powered") {
            self.powered = Some(powered);
        }
    }

    /// Forget the named properties, as if the service had never reported them.
    fn invalidate(&mut self, names: &[String]) {
        for name in names {
            match name.as_str() {
                "Address" => self.address = None,
                "Powered" => self.powered = None,
                _ => (),
            }
        }
    }
}

#[derive(Debug, Default)]
struct BluezDevice {
    address: String,
    name: String,
    alias: String,
    connected: bool,
    adapter: Option<String>,
}

impl BluezDevice {
    fn update(&mut self, props: &Properties) {
        if let Some(address) = str_property(props, "Address") {
            self.address = address;
        }
        if let Some(name) = str_property(props, "Name") {
            self.name = name;
        }
        if let Some(alias) = str_property(props, "Alias") {
            self.alias = alias;
        }
        if let Some(connected) = bool_property(props, "Connected") {
            self.connected = connected;
        }
        if let Some(adapter) = path_property(props, "Adapter") {
            self.adapter = Some(adapter);
        }
    }

    /// Forget the named properties, as if the service had never reported them.
    fn invalidate(&mut self, names: &[String]) {
        for name in names {
            match name.as_str() {
                "Address" => self.address.clear(),
                "Name" => self.name.clear(),
                "Alias" => self.alias.clear(),
                "Connected" => self.connected = false,
                "Adapter" => self.adapter = None,
                _ => (),
            }
        }
    }

    fn display_name(&self) -> &str {
        [&self.alias, &self.name, &self.address]
            .into_iter()
            .find(|s| !s.is_empty())
            .map_or("", |s| s.as_str())
    }
}

/// A device as listed by the block.
#[derive(Debug, PartialEq, Eq)]
struct ConnectedDevice {
    name: String,
    address: String,
}

/// The adapters and devices BlueZ exports, keyed by object path. Kept up to date from
/// `GetManagedObjects` and the `InterfacesAdded`, `InterfacesRemoved` and `PropertiesChanged`
/// signals, so reading it never calls into the service.
#[derive(Debug, Default)]
struct BluezState {
    adapters: HashMap<String, BluezAdapter>,
    devices: HashMap<String, BluezDevice>,
}

impl BluezState {
    fn clear(&mut self) {
        self.adapters.clear();
        self.devices.clear();
    }

    /// Returns whether anything this block tracks was added.
    fn add_interfaces(&mut self, path: &str, interfaces: &Interfaces) -> bool {
        let mut changed = false;
        for (interface, props) in interfaces {
            match interface.as_str() {
                ADAPTER_INTERFACE => {
                    self.adapters.entry(path.into()).or_default().update(props);
                    changed = true;
                }
                DEVICE_INTERFACE => {
                    self.devices.entry(path.into()).or_default().update(props);
                    changed = true;
                }
                _ => (),
            }
        }
        changed
    }

    /// Returns whether anything this block tracks was removed.
    fn remove_interfaces(&mut self, path: &str, interfaces: &[String]) -> bool {
        let mut changed = false;
        for interface in interfaces {
            changed |= match interface.as_str() {
                ADAPTER_INTERFACE => self.adapters.remove(path).is_some(),
                DEVICE_INTERFACE => self.devices.remove(path).is_some(),
                _ => false,
            };
        }
        changed
    }

    /// Store the values in `props` and forget the `invalidated` properties, as a
    /// `PropertiesChanged` signal asks. Returns whether the object is tracked by this block.
    fn update_properties(
        &mut self,
        path: &str,
        interface: &str,
        props: &Properties,
        invalidated: &[String],
    ) -> bool {
        match interface {
            ADAPTER_INTERFACE => self.adapters.get_mut(path).map(|a| {
                a.invalidate(invalidated);
                a.update(props);
            }),
            DEVICE_INTERFACE => self.devices.get_mut(path).map(|d| {
                d.invalidate(invalidated);
                d.update(props);
            }),
            _ => None,
        }
        .is_some()
    }

    /// Take every property of an interface, as `GetAll` returns them: a property missing from
    /// `props` is not set anymore.
    fn replace_properties(&mut self, path: &str, interface: &str, props: &Properties) {
        match interface {
            ADAPTER_INTERFACE => {
                if let Some(adapter) = self.adapters.get_mut(path) {
                    *adapter = BluezAdapter::default();
                    adapter.update(props);
                }
            }
            DEVICE_INTERFACE => {
                if let Some(device) = self.devices.get_mut(path) {
                    *device = BluezDevice::default();
                    device.update(props);
                }
            }
            _ => (),
        }
    }

    /// The connected devices on powered adapters (only the adapter with address `adapter_mac`,
    /// if set), sorted by Unicode case-folded name, then by address.
    fn connected_devices(&self, adapter_mac: Option<&str>) -> Vec<ConnectedDevice> {
        let mut devices: Vec<_> = self
            .devices
            .iter()
            .filter(|(path, device)| {
                if !device.connected {
                    return false;
                }
                let adapter_path = device
                    .adapter
                    .as_deref()
                    .or_else(|| path.rsplit_once('/').map(|(parent, _)| parent));
                let Some(adapter) = adapter_path.and_then(|p| self.adapters.get(p)) else {
                    return false;
                };
                adapter.powered != Some(false)
                    && adapter_mac.is_none_or(|mac| adapter.address.as_deref() == Some(mac))
            })
            .map(|(_, device)| ConnectedDevice {
                name: device.display_name().into(),
                address: device.address.clone(),
            })
            .collect();
        devices.sort_by_cached_key(|d| (case_fold(&d.name), d.address.clone()));
        devices
    }
}

/// Unicode full case folding (the C and F mappings of `CaseFolding.txt`), built from the standard
/// library's case mappings: a character folds to the lowercase of the uppercase of its lowercase,
/// except that dotless i has no folding and Cherokee folds to its uppercase letters. Unlike
/// `to_lowercase`, this makes final sigma equal sigma and `ß` equal `ss`.
fn case_fold(s: &str) -> String {
    let mut folded = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            'ı' => folded.push(c),
            '\u{13A0}'..='\u{13FF}' | '\u{AB70}'..='\u{ABBF}' => folded.extend(c.to_uppercase()),
            _ => folded.extend(
                c.to_lowercase()
                    .flat_map(char::to_uppercase)
                    .flat_map(char::to_lowercase),
            ),
        }
    }
    folded
}

/// Whether the block keeps `property` of `interface`; re-reading the interface after any other
/// property is invalidated (such as `RSSI` when a discovery ends) would change nothing.
fn is_tracked_property(interface: &str, property: &str) -> bool {
    match interface {
        ADAPTER_INTERFACE => matches!(property, "Address" | "Powered"),
        DEVICE_INTERFACE => matches!(
            property,
            "Address" | "Name" | "Alias" | "Connected" | "Adapter"
        ),
        _ => false,
    }
}

/// A method call sent by `BluezObjects` whose reply has not been read yet.
#[derive(Debug)]
enum Request {
    /// `GetManagedObjects`, sent whenever the service (re)appears.
    Objects,
    /// `GetAll` for an interface whose properties the service invalidated.
    Properties { path: String, interface: String },
}

/// Follows every BlueZ adapter and device through one D-Bus connection. The objects are read
/// once whenever the service (re)appears; after that, every update comes from signals.
struct BluezObjects {
    conn: zbus::Connection,
    /// Every message received on `conn`, in order: signals and the replies to `requests`. A
    /// single stream keeps an `InterfacesAdded` and the `PropertiesChanged` that follows it from
    /// being applied out of order.
    ///
    /// It must never sit unpolled while a reply is awaited: zbus delivers every incoming message
    /// to every stream in turn and waits while a stream's queue is full, so a burst of signals
    /// would hold back the reply forever. Method calls are therefore sent without waiting, and
    /// their replies are read from this stream like any other message.
    messages: MessageStream,
    /// Unique bus name of the current `org.bluez` owner, if the service is running.
    owner: Option<String>,
    /// Calls sent on `conn` that are still waiting for their reply, by serial number.
    requests: HashMap<NonZeroU32, Request>,
    state: BluezState,
}

impl BluezObjects {
    async fn new(adapter_mac: Option<String>) -> Result<Self> {
        debug!("following all devices (adapter filter: {adapter_mac:?})");
        let conn = new_system_dbus_connection().await?;
        // Subscribe before adding the match rules and reading the objects, so that nothing
        // that happens in between is missed.
        let messages = MessageStream::from(&conn);
        let dbus = DBusProxy::new(&conn)
            .await
            .error("Failed to create DBusProxy")?;
        for rule in [
            MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender("org.freedesktop.DBus")
                .and_then(|x| x.interface("org.freedesktop.DBus"))
                .and_then(|x| x.member("NameOwnerChanged"))
                .and_then(|x| x.add_arg(BLUEZ_SERVICE))
                .unwrap()
                .build(),
            MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender(BLUEZ_SERVICE)
                .and_then(|x| x.interface(OBJECT_MANAGER_INTERFACE))
                .and_then(|x| x.path("/"))
                .unwrap()
                .build(),
            MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender(BLUEZ_SERVICE)
                .and_then(|x| x.interface(PROPERTIES_INTERFACE))
                .and_then(|x| x.member("PropertiesChanged"))
                .and_then(|x| x.path_namespace("/org/bluez"))
                .unwrap()
                .build(),
        ] {
            dbus.add_match_rule(rule)
                .await
                .error("Failed to add match rule")?;
        }

        let mut this = Self {
            conn,
            messages,
            owner: None,
            requests: HashMap::new(),
            state: BluezState::default(),
        };
        this.reload(BLUEZ_SERVICE).await;
        // Start with the objects the service exports (or none, if it is not running).
        this.wait_for_change().await?;
        Ok(this)
    }

    /// Send a method call; its reply is handled when it comes in on `messages`.
    async fn send_request<B>(
        &mut self,
        destination: &str,
        path: &str,
        interface: &str,
        method: &str,
        body: &B,
        request: Request,
    ) where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        let message = zbus::Message::method_call(path, method)
            .and_then(|b| b.destination(destination))
            .and_then(|b| b.interface(interface))
            .and_then(|b| b.build(body));
        let message = match message {
            Ok(message) => message,
            Err(err) => {
                debug!("could not build {method} for {path}: {err}");
                return;
            }
        };
        let serial = message.primary_header().serial_num();
        match self.conn.send(&message).await {
            Ok(()) => {
                self.requests.insert(serial, request);
            }
            Err(err) => {
                debug!("could not send {method} to {path}: {err}");
            }
        }
    }

    /// Forget the current objects and ask `destination` for the ones it exports. Until the reply
    /// comes in, the state is not reported; if the service is not running, it stays empty until
    /// the service appears.
    async fn reload(&mut self, destination: &str) {
        self.state.clear();
        // Replies to calls made before are about objects that are gone now.
        self.requests.clear();
        self.send_request(
            destination,
            "/",
            OBJECT_MANAGER_INTERFACE,
            "GetManagedObjects",
            &(),
            Request::Objects,
        )
        .await;
    }

    /// Re-read an interface whose properties the service invalidated without sending their
    /// values. Properties missing from the reply stay forgotten.
    async fn refresh(&mut self, path: &str, interface: &str) {
        let Some(owner) = self.owner.clone() else {
            return;
        };
        let request = Request::Properties {
            path: path.into(),
            interface: interface.into(),
        };
        self.send_request(
            &owner,
            path,
            PROPERTIES_INTERFACE,
            "GetAll",
            &(interface,),
            request,
        )
        .await;
    }

    /// Wait until the state changes. A change is reported once every reply it waits for has
    /// been applied, so that a half-updated state (such as an empty list while the objects of a
    /// new owner are being read) is never shown.
    async fn wait_for_change(&mut self) -> Result<()> {
        let mut changed = false;
        loop {
            let message = self
                .messages
                .next()
                .await
                .error("D-Bus message stream ended unexpectedly")?;
            match message {
                Ok(message) => changed |= self.handle(&message).await,
                Err(err) => {
                    debug!("D-Bus error: {err}");
                }
            }
            if changed && self.requests.is_empty() {
                return Ok(());
            }
        }
    }

    /// Apply a message to the state. Returns whether the state may have changed.
    async fn handle(&mut self, message: &zbus::Message) -> bool {
        match message.message_type() {
            zbus::message::Type::Signal => self.handle_signal(message).await,
            zbus::message::Type::MethodReturn | zbus::message::Type::Error => {
                self.handle_reply(message)
            }
            _ => false,
        }
    }

    /// Apply the reply to one of `requests`. Returns whether the state may have changed.
    fn handle_reply(&mut self, message: &zbus::Message) -> bool {
        let header = message.header();
        let Some(request) = header
            .reply_serial()
            .and_then(|serial| self.requests.remove(&serial))
        else {
            return false;
        };
        let is_error = message.message_type() == zbus::message::Type::Error;
        let body = message.body();
        match request {
            Request::Objects => {
                // A reply that does not come from the bus itself (which answers when the
                // service is not running) comes from the service, even if it is an error: follow
                // its signals from now on, so that a failed first read is not fatal.
                if let Some(sender) = header.sender()
                    && sender != "org.freedesktop.DBus"
                {
                    self.owner = Some(sender.to_string());
                }
                if is_error {
                    debug!(
                        "could not get the list of managed objects: {:?}",
                        header.error_name()
                    );
                    return true;
                }
                match body.deserialize::<HashMap<OwnedObjectPath, Interfaces>>() {
                    Ok(objects) => {
                        for (path, interfaces) in &objects {
                            self.state.add_interfaces(path.as_str(), interfaces);
                        }
                    }
                    Err(err) => {
                        debug!("unexpected managed objects: {err}");
                    }
                }
                true
            }
            Request::Properties { path, interface } => {
                if self.owner.as_deref() != header.sender().map(|s| s.as_str()) {
                    return false;
                }
                if is_error {
                    debug!("could not refresh {path}: {:?}", header.error_name());
                    return false;
                }
                match body.deserialize::<Properties>() {
                    Ok(props) => {
                        self.state.replace_properties(&path, &interface, &props);
                        true
                    }
                    Err(err) => {
                        debug!("unexpected properties of {path}: {err}");
                        false
                    }
                }
            }
        }
    }

    /// Apply a signal to the state. Returns whether the state may have changed.
    async fn handle_signal(&mut self, message: &zbus::Message) -> bool {
        let header = message.header();
        let (Some(interface), Some(member), Some(sender)) =
            (header.interface(), header.member(), header.sender())
        else {
            return false;
        };
        let body = message.body();

        if interface == "org.freedesktop.DBus" && member == "NameOwnerChanged" {
            if sender != "org.freedesktop.DBus" {
                return false;
            }
            let Ok((name, _old_owner, new_owner)) = body.deserialize::<(&str, &str, &str)>() else {
                return false;
            };
            if name != BLUEZ_SERVICE {
                return false;
            }
            self.owner = None;
            self.state.clear();
            self.requests.clear();
            if new_owner.is_empty() {
                debug!("org.bluez disappeared");
            } else {
                debug!("org.bluez appeared as {new_owner}");
                self.owner = Some(new_owner.into());
                self.reload(new_owner).await;
            }
            return true;
        }

        // Anything else must come from the current owner of org.bluez.
        if self.owner.as_deref() != Some(sender.as_str()) {
            return false;
        }
        let Some(path) = header.path() else {
            return false;
        };

        match (interface.as_str(), member.as_str()) {
            (OBJECT_MANAGER_INTERFACE, "InterfacesAdded") => body
                .deserialize::<(OwnedObjectPath, Interfaces)>()
                .is_ok_and(|(path, interfaces)| {
                    self.state.add_interfaces(path.as_str(), &interfaces)
                }),
            (OBJECT_MANAGER_INTERFACE, "InterfacesRemoved") => body
                .deserialize::<(OwnedObjectPath, Vec<String>)>()
                .is_ok_and(|(path, interfaces)| {
                    self.state.remove_interfaces(path.as_str(), &interfaces)
                }),
            (PROPERTIES_INTERFACE, "PropertiesChanged") => {
                let Ok((interface, changed, invalidated)) =
                    body.deserialize::<(String, Properties, Vec<String>)>()
                else {
                    return false;
                };
                let path = path.as_str();
                if !self
                    .state
                    .update_properties(path, &interface, &changed, &invalidated)
                {
                    return false;
                }
                if invalidated
                    .iter()
                    .any(|name| is_tracked_property(&interface, name))
                {
                    self.refresh(path, &interface).await;
                }
                true
            }
            _ => false,
        }
    }
}

#[zbus::proxy(interface = "org.bluez.Device1", default_service = "org.bluez")]
trait Device1 {
    fn connect(&self) -> zbus::Result<()>;
    fn disconnect(&self) -> zbus::Result<()>;

    #[zbus(property)]
    fn connected(&self) -> zbus::Result<bool>;

    #[zbus(property)]
    fn name(&self) -> zbus::Result<String>;

    #[zbus(property)]
    fn icon(&self) -> zbus::Result<String>;
}

#[zbus::proxy(interface = "org.bluez.Battery1", default_service = "org.bluez")]
trait Battery1 {
    #[zbus(property)]
    fn percentage(&self) -> zbus::Result<u8>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Value as DBusValue;

    fn config() -> Config {
        Config {
            mac: Some("00:00:00:00:00:00".into()),
            adapter_mac: None,
            format: Default::default(),
            disconnected_format: Default::default(),
            battery_state: None,
        }
    }

    #[test]
    fn plan_declares_every_state_with_its_icons() {
        let plan = prepare(&config()).unwrap();
        let ids: Vec<_> = plan.outputs().map(|o| o.id()).collect();
        assert_eq!(ids, ["connected", "disconnected", "unavailable"]);

        // The device values (icon, battery_icon) are set regardless of the
        // connection state, so both connected and disconnected declare them.
        for id in ["connected", "disconnected"] {
            let output = plan.output(id).unwrap();
            let choices = output.output().choices_for("icon").unwrap();
            for name in DEVICE_ICONS {
                assert!(choices.permits(name), "{id} must permit {name}");
            }
            assert_eq!(output.single_icon("battery_icon").unwrap(), "bat");
        }

        let unavailable = plan.output("unavailable").unwrap();
        assert_eq!(unavailable.single_icon("icon").unwrap(), "bluetooth");
        assert!(
            unavailable.output().choices_for("battery_icon").is_none(),
            "battery values are never set when the device is unavailable"
        );
    }

    #[test]
    fn every_device_icon_is_declared() {
        // Tie the runtime chooser to the declared set.
        for input in [
            Some("audio-card"),
            Some("audio-headset"),
            Some("audio-headphones"),
            Some("input-gaming"),
            Some("input-keyboard"),
            Some("input-mouse"),
            Some("something-else"),
            None,
        ] {
            assert!(DEVICE_ICONS.contains(&device_icon(input)));
        }
    }

    fn props(entries: &[(&str, DBusValue<'_>)]) -> Properties {
        entries
            .iter()
            .map(|(k, v)| ((*k).into(), v.try_to_owned().unwrap()))
            .collect()
    }

    fn adapter(address: &str, powered: bool) -> Interfaces {
        [(
            ADAPTER_INTERFACE.into(),
            props(&[
                ("Address", DBusValue::from(address)),
                ("Powered", DBusValue::from(powered)),
            ]),
        )]
        .into()
    }

    fn device(
        adapter: &str,
        address: &str,
        name: &str,
        alias: &str,
        connected: bool,
    ) -> Interfaces {
        [(
            DEVICE_INTERFACE.into(),
            props(&[
                ("Address", DBusValue::from(address)),
                ("Name", DBusValue::from(name)),
                ("Alias", DBusValue::from(alias)),
                ("Connected", DBusValue::from(connected)),
                (
                    "Adapter",
                    DBusValue::from(ObjectPath::try_from(adapter).unwrap()),
                ),
            ]),
        )]
        .into()
    }

    fn names(state: &BluezState, adapter_mac: Option<&str>) -> Vec<String> {
        state
            .connected_devices(adapter_mac)
            .into_iter()
            .map(|d| d.name)
            .collect()
    }

    #[test]
    fn all_devices_plan_uses_the_generic_icon() {
        let config = Config {
            mac: None,
            ..config()
        };
        let plan = prepare(&config).unwrap();
        let ids: Vec<_> = plan.outputs().map(|o| o.id()).collect();
        assert_eq!(ids, ["devices", "no_devices"]);
        for id in ids {
            assert_eq!(
                plan.output(id).unwrap().single_icon("icon").unwrap(),
                "bluetooth"
            );
        }
    }

    #[test]
    fn all_devices_lists_connected_devices_sorted() {
        let mut state = BluezState::default();
        state.add_interfaces("/org/bluez/hci0", &adapter("00:00:00:00:00:A0", true));
        state.add_interfaces("/org/bluez/hci1", &adapter("00:00:00:00:00:A1", true));
        for (adapter, address, name, alias, connected) in [
            ("/org/bluez/hci0", "AA:00:00:00:00:02", "mouse", "", true),
            ("/org/bluez/hci0", "AA:00:00:00:00:01", "x", "Mouse", true),
            ("/org/bluez/hci1", "AA:00:00:00:00:03", "", "", true),
            (
                "/org/bluez/hci1",
                "AA:00:00:00:00:04",
                "Zebra",
                "apple",
                true,
            ),
            ("/org/bluez/hci1", "AA:00:00:00:00:05", "Off", "Off", false),
        ] {
            let path = format!("{adapter}/dev_{}", address.replace(':', "_"));
            state.add_interfaces(&path, &device(adapter, address, name, alias, connected));
        }
        assert_eq!(
            names(&state, None),
            ["AA:00:00:00:00:03", "apple", "Mouse", "mouse"]
        );
        assert_eq!(names(&state, Some("00:00:00:00:00:A0")), ["Mouse", "mouse"]);
        assert!(names(&state, Some("00:00:00:00:00:FF")).is_empty());
    }

    #[test]
    fn all_devices_sorts_by_case_folded_name() {
        let mut state = BluezState::default();
        state.add_interfaces("/org/bluez/hci0", &adapter("00:00:00:00:00:A0", true));
        // Each pair folds to the same name, so the address decides; lowercasing would keep
        // them apart and order them the other way.
        for (address, alias) in [
            ("AA:00:00:00:00:01", "Σ"),
            ("AA:00:00:00:00:02", "ς"),
            ("AA:00:00:00:00:03", "Straße"),
            ("AA:00:00:00:00:04", "STRASSE"),
        ] {
            let path = format!("/org/bluez/hci0/dev_{}", address.replace(':', "_"));
            state.add_interfaces(&path, &device("/org/bluez/hci0", address, "", alias, true));
        }
        assert_eq!(names(&state, None), ["Straße", "STRASSE", "Σ", "ς"]);

        assert_eq!(
            case_fold("ΣΑΣ ς Straße ẞ ﬀ İ"),
            "σασ σ strasse ss ff i\u{307}"
        );
        assert_eq!(case_fold("ı I"), "ı i");
        assert_eq!(
            case_fold("\u{13A0}\u{AB70}\u{13F8}"),
            "\u{13A0}\u{13A0}\u{13F0}"
        );
    }

    #[test]
    fn all_devices_follows_changes() {
        let mut state = BluezState::default();
        let dev = "/org/bluez/hci0/dev_AA_00_00_00_00_01";
        state.add_interfaces("/org/bluez/hci0", &adapter("00:00:00:00:00:A0", true));
        state.add_interfaces(
            dev,
            &device("/org/bluez/hci0", "AA:00:00:00:00:01", "Name", "", false),
        );
        assert!(names(&state, None).is_empty());

        assert!(state.update_properties(
            dev,
            DEVICE_INTERFACE,
            &props(&[("Connected", DBusValue::from(true))]),
            &[],
        ));
        assert_eq!(names(&state, None), ["Name"]);

        state.update_properties(
            dev,
            DEVICE_INTERFACE,
            &props(&[("Alias", DBusValue::from("Alias"))]),
            &[],
        );
        assert_eq!(names(&state, None), ["Alias"]);

        state.update_properties(
            "/org/bluez/hci0",
            ADAPTER_INTERFACE,
            &props(&[("Powered", DBusValue::from(false))]),
            &[],
        );
        assert!(names(&state, None).is_empty());
        state.update_properties(
            "/org/bluez/hci0",
            ADAPTER_INTERFACE,
            &props(&[("Powered", DBusValue::from(true))]),
            &[],
        );
        assert_eq!(names(&state, None), ["Alias"]);

        // An invalidated property is forgotten until the service reports it again.
        state.update_properties(dev, DEVICE_INTERFACE, &props(&[]), &["Alias".into()]);
        assert_eq!(names(&state, None), ["Name"]);
        // A GetAll reply is complete: the optional Name it leaves out is gone.
        state.replace_properties(
            dev,
            DEVICE_INTERFACE,
            &props(&[
                ("Address", DBusValue::from("AA:00:00:00:00:01")),
                ("Alias", DBusValue::from("")),
                ("Connected", DBusValue::from(true)),
                (
                    "Adapter",
                    DBusValue::from(ObjectPath::try_from("/org/bluez/hci0").unwrap()),
                ),
            ]),
        );
        assert_eq!(names(&state, None), ["AA:00:00:00:00:01"]);

        assert!(state.remove_interfaces(dev, &[DEVICE_INTERFACE.into()]));
        assert!(names(&state, None).is_empty());
        assert!(!state.update_properties(
            dev,
            DEVICE_INTERFACE,
            &props(&[("Connected", DBusValue::from(true))]),
            &[],
        ));
    }

    #[test]
    fn only_tracked_invalidations_are_read_again() {
        for name in ["Address", "Name", "Alias", "Connected", "Adapter"] {
            assert!(is_tracked_property(DEVICE_INTERFACE, name), "{name}");
        }
        for name in ["Address", "Powered"] {
            assert!(is_tracked_property(ADAPTER_INTERFACE, name), "{name}");
        }
        // BlueZ invalidates these on every device when a discovery ends.
        assert!(!is_tracked_property(DEVICE_INTERFACE, "RSSI"));
        assert!(!is_tracked_property(DEVICE_INTERFACE, "TxPower"));
        assert!(!is_tracked_property(ADAPTER_INTERFACE, "Discovering"));
        assert!(!is_tracked_property("org.bluez.Battery1", "Percentage"));
    }

    #[test]
    fn unavailable_shares_the_disconnected_format() {
        let config = Config {
            disconnected_format: " off ".parse().unwrap(),
            ..config()
        };
        let plan = prepare(&config).unwrap();
        for id in ["disconnected", "unavailable"] {
            assert!(!plan.output(id).unwrap().format().contains_key("icon"));
        }
        assert!(
            plan.output("connected")
                .unwrap()
                .format()
                .contains_key("icon")
        );
    }
}
