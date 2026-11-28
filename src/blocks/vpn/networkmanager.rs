//! VPN status from NetworkManager over the system bus.
//!
//! The driver keeps the D-Bus signals it needs subscribed for its whole
//! lifetime and only talks to NetworkManager when one of them arrives (or
//! when the user clicks the block). It never polls: the only timed work is a
//! short, bounded series of re-reads after a failed read, and reconnecting
//! while the system bus itself is gone.

use std::collections::HashMap;
use std::pin::Pin;

use zbus::fdo::{DBusProxy, NameOwnerChangedStream, PropertiesProxy};
use zbus::names::{BusName, InterfaceName};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::{MatchRule, MessageStream};

use crate::blocks::prelude::*;

use super::{Driver, Status};

make_log_macro!(debug, "vpn");

const NM_SERVICE: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const ACTIVE_CONNECTIONS_PATH: &str = "/org/freedesktop/NetworkManager/ActiveConnection";
const MANAGER_INTERFACE: &str = "org.freedesktop.NetworkManager";
const ACTIVE_INTERFACE: &str = "org.freedesktop.NetworkManager.Connection.Active";

/// `NMActiveConnectionState` values.
const STATE_ACTIVATING: u32 = 1;
const STATE_ACTIVATED: u32 = 2;

/// Connection types shown by the block, highest priority first. A `tun` or
/// `tap` connection only counts when its device is not a port of another
/// device.
const PRIORITIES: [&[&str]; 3] = [&["vpn"], &["wireguard"], &["tun", "tap"]];

/// Connection types the toggle action may bring up.
const TOGGLE_TYPES: [&str; 2] = ["vpn", "wireguard"];

/// Properties whose changes can alter what the block shows, per interface.
/// The match rules already limit signals to these two interfaces; changes to
/// their other properties (connectivity, IP configuration, ...) do not cause
/// a re-read either.
const WATCHED_PROPERTIES: [(&str, &[&str]); 2] = [
    (MANAGER_INTERFACE, &["ActiveConnections"]),
    (
        ACTIVE_INTERFACE,
        &["State", "Id", "Type", "Controller", "Master"],
    ),
];

/// A read can fail transiently: NetworkManager exports its objects right
/// after it takes its bus name, and an object can briefly refuse a request.
/// A failed read that follows a signal (or a fresh connection) is retried this
/// many times, `RETRY_DELAY` apart, before waiting for the next signal.
const READ_RETRIES: u8 = 10;
const RETRY_DELAY: Duration = Duration::from_millis(200);

/// How often to try again when the system bus itself cannot be reached, so a
/// bus that appears late or restarts is picked up promptly. A failed attempt
/// only looks for the bus socket; nothing reaches NetworkManager.
const RECONNECT_DELAY: Duration = Duration::from_millis(500);

#[derive(Debug)]
struct ActiveConnection {
    path: OwnedObjectPath,
    id: String,
    kind: String,
    state: u32,
    is_port: bool,
}

impl ActiveConnection {
    fn qualifies(&self, kinds: &[&str]) -> bool {
        kinds.contains(&self.kind.as_str())
            && matches!(self.state, STATE_ACTIVATING | STATE_ACTIVATED)
            && !(matches!(self.kind.as_str(), "tun" | "tap") && self.is_port)
    }
}

/// Pick the connection to show from NetworkManager's active connection list,
/// keeping NetworkManager's order within each priority.
fn choose(actives: Vec<ActiveConnection>) -> Option<ActiveConnection> {
    let index = PRIORITIES
        .iter()
        .find_map(|kinds| actives.iter().position(|a| a.qualifies(kinds)))?;
    actives.into_iter().nth(index)
}

/// Of saved profiles given as `(type, timestamp, profile)` in NetworkManager's
/// order, the `vpn` or `wireguard` one that was connected most recently. A
/// profile that was never connected (timestamp 0) still counts, behind every
/// one that was; on equal timestamps the first listed wins.
fn most_recent<T>(profiles: impl IntoIterator<Item = (String, u64, T)>) -> Option<T> {
    let mut best: Option<(u64, T)> = None;
    for (kind, timestamp, profile) in profiles {
        if TOGGLE_TYPES.contains(&kind.as_str())
            && best.as_ref().is_none_or(|(t, _)| timestamp > *t)
        {
            best = Some((timestamp, profile));
        }
    }
    best.map(|(_, profile)| profile)
}

/// Whether a `PropertiesChanged` signal from NetworkManager's object tree
/// touches a property in `WATCHED_PROPERTIES`. A signal that cannot be parsed
/// is treated as relevant.
fn is_relevant(msg: &zbus::Message) -> bool {
    let body = msg.body();
    let Ok((interface, changed, invalidated)) =
        body.deserialize::<(&str, HashMap<&str, Value<'_>>, Vec<&str>)>()
    else {
        return true;
    };
    let Some((_, watched)) = WATCHED_PROPERTIES.iter().find(|(i, _)| *i == interface) else {
        return false;
    };
    changed
        .keys()
        .chain(invalidated.iter())
        .any(|key| watched.contains(key))
}

/// A match rule for `PropertiesChanged` on `interface`, sent either by the
/// manager object itself or by the objects below `…/ActiveConnection`.
fn properties_rule(
    active_connections: bool,
    interface: &'static str,
) -> Result<MatchRule<'static>> {
    let builder = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.freedesktop.DBus.Properties")
        .and_then(|r| r.member("PropertiesChanged"));
    let builder = if active_connections {
        builder.and_then(|r| r.path_namespace(ACTIVE_CONNECTIONS_PATH))
    } else {
        builder.and_then(|r| r.path(NM_PATH))
    };
    Ok(builder
        .and_then(|r| r.arg(0, interface))
        .error("Failed to build the NetworkManager match rule")?
        .build())
}

type PropertyChanges = Pin<Box<dyn Stream<Item = zbus::Result<zbus::Message>> + Send + Sync>>;

struct Bus {
    conn: zbus::Connection,
    owner_changes: NameOwnerChangedStream,
    property_changes: PropertyChanges,
}

enum Event {
    Changed,
    Appeared,
    Vanished,
    BusLost,
}

impl Bus {
    async fn connect() -> Result<(Self, bool)> {
        let conn = new_system_dbus_connection().await?;
        let dbus = DBusProxy::new(&conn)
            .await
            .error("Failed to create DBusProxy")?;
        // Subscribe before looking at the current state, so no change can
        // slip in between.
        let owner_changes = dbus
            .receive_name_owner_changed_with_args(&[(0, NM_SERVICE)])
            .await
            .error("Failed to monitor NetworkManager's bus name")?;
        // Only the manager object and the active connections matter. The bus
        // filters on the interface (arg0) too, so the much busier device,
        // access point and IP configuration objects never wake the block.
        let manager_changes =
            MessageStream::for_match_rule(properties_rule(false, MANAGER_INTERFACE)?, &conn, None)
                .await
                .error("Failed to monitor NetworkManager")?;
        let active_changes =
            MessageStream::for_match_rule(properties_rule(true, ACTIVE_INTERFACE)?, &conn, None)
                .await
                .error("Failed to monitor NetworkManager's active connections")?;
        // A read error from the connection is passed through: it is the last
        // item before the connection's streams end.
        let property_changes: PropertyChanges = Box::pin(
            futures::stream::select(manager_changes, active_changes)
                .filter(|msg| std::future::ready(msg.as_ref().map_or(true, is_relevant))),
        );
        let present = dbus
            .name_has_owner(BusName::try_from(NM_SERVICE).unwrap())
            .await
            .error("Failed to look up NetworkManager")?;
        Ok((
            Self {
                conn,
                owner_changes,
                property_changes,
            },
            present,
        ))
    }

    async fn next_event(&mut self) -> Event {
        select! {
            change = self.owner_changes.next() => {
                let Some(change) = change else { return Event::BusLost };
                match change.args() {
                    Ok(args) if args.new_owner().is_some() => Event::Appeared,
                    Ok(_) => Event::Vanished,
                    Err(_) => Event::Changed,
                }
            }
            change = self.property_changes.next_debounced() => match change {
                Some(Ok(_)) => Event::Changed,
                Some(Err(_)) | None => Event::BusLost,
            },
        }
    }

    async fn properties(&self, path: ObjectPath<'_>) -> Result<PropertiesProxy<'static>> {
        PropertiesProxy::builder(&self.conn)
            .destination(NM_SERVICE)
            .and_then(|b| b.path(path.into_owned()))
            .error("Invalid NetworkManager object path")?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
            .error("Failed to create PropertiesProxy")
    }

    async fn manager(&self) -> Result<NetworkManagerProxy<'_>> {
        NetworkManagerProxy::builder(&self.conn)
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
            .error("Failed to create NetworkManager proxy")
    }

    /// NetworkManager's active connections, in its order, and whether every
    /// listed one could be read.
    async fn active_connections(&self) -> Result<(Vec<ActiveConnection>, bool)> {
        let paths: Vec<OwnedObjectPath> = self
            .properties(ObjectPath::from_static_str_unchecked(NM_PATH))
            .await?
            .get(
                InterfaceName::from_static_str_unchecked(MANAGER_INTERFACE),
                "ActiveConnections",
            )
            .await
            .error("Failed to get NetworkManager's active connections")?
            .try_into()
            .error("Invalid ActiveConnections property")?;

        let mut actives = Vec::with_capacity(paths.len());
        let mut complete = true;
        for path in paths {
            // An active connection can go away while the list is read, or a
            // read can fail transiently. Show what could be read and let the
            // caller read again.
            match self.active_connection(&path).await {
                Ok(active) => actives.push(active),
                Err(e) => {
                    debug!("could not read active connection {}: {e}", path.as_str());
                    complete = false;
                }
            }
        }
        Ok((actives, complete))
    }

    async fn active_connection(&self, path: &OwnedObjectPath) -> Result<ActiveConnection> {
        let props = self
            .properties(path.as_ref())
            .await?
            .get_all(InterfaceName::from_static_str_unchecked(ACTIVE_INTERFACE))
            .await
            .error("Failed to get the active connection's properties")?;
        let string = |key: &str| -> String {
            props
                .get(key)
                .and_then(|v| v.downcast_ref::<&str>().ok())
                .unwrap_or_default()
                .to_owned()
        };
        // NetworkManager 1.44 renamed `Master` to `Controller`; older
        // versions only know the former.
        let controller = ["Controller", "Master"].into_iter().find_map(|key| {
            props
                .get(key)
                .and_then(|v| v.downcast_ref::<ObjectPath>().ok())
        });
        Ok(ActiveConnection {
            path: path.clone(),
            id: string("Id"),
            kind: string("Type"),
            state: props
                .get("State")
                .and_then(|v| v.downcast_ref::<u32>().ok())
                .unwrap_or_default(),
            is_port: controller.is_some_and(|c| c.as_str() != "/"),
        })
    }

    /// The saved `vpn` or `wireguard` profile with the most recent connection
    /// timestamp.
    async fn most_recent_profile(&self) -> Result<Option<OwnedObjectPath>> {
        let settings = SettingsProxy::builder(&self.conn)
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
            .error("Failed to create NetworkManager settings proxy")?;
        let paths = settings
            .list_connections()
            .await
            .error("Failed to list NetworkManager connections")?;

        let mut profiles = Vec::with_capacity(paths.len());
        for path in paths {
            let profile = SettingsConnectionProxy::builder(&self.conn)
                .path(path.clone())
                .error("Invalid NetworkManager connection path")?
                .cache_properties(zbus::proxy::CacheProperties::No)
                .build()
                .await
                .error("Failed to create NetworkManager connection proxy")?;
            let Ok(settings) = profile.get_settings().await else {
                debug!("could not read settings of {}", path.as_str());
                continue;
            };
            let Some(connection) = settings.get("connection") else {
                continue;
            };
            let kind = connection
                .get("type")
                .and_then(|v| v.downcast_ref::<&str>().ok())
                .unwrap_or_default()
                .to_owned();
            let timestamp = connection
                .get("timestamp")
                .and_then(|v| v.downcast_ref::<u64>().ok())
                .unwrap_or(0);
            profiles.push((kind, timestamp, path));
        }
        Ok(most_recent(profiles))
    }
}

pub struct NetworkManagerDriver {
    bus: Option<Bus>,
    /// Whether NetworkManager owns its bus name.
    present: bool,
    /// The active connection the block currently shows.
    shown: Option<OwnedObjectPath>,
    /// Whether the next read follows a signal or a new connection, rather
    /// than a retry.
    fresh: bool,
    /// Reads left after a failed one.
    retries: u8,
}

impl NetworkManagerDriver {
    pub async fn new() -> Self {
        let mut driver = Self {
            bus: None,
            present: false,
            shown: None,
            fresh: true,
            retries: 0,
        };
        driver.connect().await;
        driver
    }

    async fn connect(&mut self) -> bool {
        match Bus::connect().await {
            Ok((bus, present)) => {
                self.bus = Some(bus);
                self.present = present;
                self.fresh = true;
                true
            }
            Err(e) => {
                debug!("system bus unavailable: {e}");
                self.bus = None;
                self.present = false;
                false
            }
        }
    }

    /// The status to show, and whether it rests on a complete read.
    async fn read_status(&mut self) -> Result<(Status, bool)> {
        let Some(bus) = self.bus.as_ref().filter(|_| self.present) else {
            return Ok((Status::Disconnected { profile: None }, true));
        };
        let (actives, complete) = bus.active_connections().await?;
        let Some(active) = choose(actives) else {
            return Ok((Status::Disconnected { profile: None }, complete));
        };
        let name = Some(active.id);
        self.shown = Some(active.path);
        let status = match active.state {
            STATE_ACTIVATED => Status::Connected {
                country: None,
                country_flag: None,
                profile: None,
                name,
            },
            _ => Status::Connecting {
                profile: None,
                name,
            },
        };
        Ok((status, complete))
    }

    /// After a failed or partial read, read again soon. A read that follows
    /// a signal starts a new series of retries; a retry uses one up, so a
    /// lasting failure stops after `READ_RETRIES` reads until the next signal.
    fn schedule_retry(&mut self, fresh: bool) {
        self.retries = if fresh {
            READ_RETRIES
        } else {
            self.retries.saturating_sub(1)
        };
    }
}

#[async_trait]
impl Driver for NetworkManagerDriver {
    async fn get_status(&mut self) -> Result<Status> {
        self.shown = None;
        let fresh = std::mem::take(&mut self.fresh);
        match self.read_status().await {
            Ok((status, complete)) => {
                if complete {
                    self.retries = 0;
                } else {
                    self.schedule_retry(fresh);
                }
                Ok(status)
            }
            Err(e) => {
                // NetworkManager might be starting up or going away; neither
                // is an error worth showing.
                debug!("could not read NetworkManager state: {e}");
                self.schedule_retry(fresh);
                Ok(Status::Disconnected { profile: None })
            }
        }
    }

    async fn toggle_connection(&mut self, status: &Status) -> Result<()> {
        let Some(bus) = self.bus.as_ref().filter(|_| self.present) else {
            return Ok(());
        };
        let result = match (status, &self.shown) {
            (Status::Connected { .. } | Status::Connecting { .. }, Some(active)) => {
                match bus.manager().await {
                    Ok(manager) => manager
                        .deactivate_connection(active.as_ref())
                        .await
                        .error("Failed to deactivate the connection"),
                    Err(e) => Err(e),
                }
            }
            (Status::Disconnected { .. }, _) => match bus.most_recent_profile().await {
                Ok(Some(profile)) => match bus.manager().await {
                    Ok(manager) => {
                        let none = ObjectPath::from_static_str_unchecked("/");
                        manager
                            .activate_connection(profile.as_ref(), none.clone(), none)
                            .await
                            .map(drop)
                            .error("Failed to activate the connection")
                    }
                    Err(e) => Err(e),
                },
                Ok(None) => {
                    debug!("no saved vpn or wireguard connection to activate");
                    Ok(())
                }
                Err(e) => Err(e),
            },
            _ => Ok(()),
        };
        // The outcome shows up through NetworkManager's signals; a failed
        // request leaves the block as it is.
        if let Err(e) = result {
            debug!("toggle failed: {e}");
        }
        Ok(())
    }

    async fn wait_for_change(&mut self, _interval: Seconds) -> Result<()> {
        loop {
            let Some(bus) = self.bus.as_mut() else {
                sleep(RECONNECT_DELAY).await;
                if self.connect().await {
                    return Ok(());
                }
                continue;
            };

            let event = if self.retries > 0 {
                match tokio::time::timeout(RETRY_DELAY, bus.next_event()).await {
                    Ok(event) => event,
                    Err(_) => return Ok(()),
                }
            } else {
                bus.next_event().await
            };

            self.fresh = true;
            match event {
                Event::Changed => {
                    if !self.present {
                        // A signal from NetworkManager's object tree means
                        // it is there, even if its name change was missed.
                        self.present = true;
                    }
                }
                Event::Appeared => {
                    self.present = true;
                }
                Event::Vanished => {
                    self.present = false;
                    self.retries = 0;
                }
                Event::BusLost => {
                    debug!("lost the system bus connection");
                    self.bus = None;
                    self.present = false;
                    self.retries = 0;
                }
            }
            return Ok(());
        }
    }
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait NetworkManager {
    fn activate_connection(
        &self,
        connection: ObjectPath<'_>,
        device: ObjectPath<'_>,
        specific_object: ObjectPath<'_>,
    ) -> zbus::Result<OwnedObjectPath>;

    fn deactivate_connection(&self, active_connection: ObjectPath<'_>) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Settings",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager/Settings"
)]
trait Settings {
    fn list_connections(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Settings.Connection",
    default_service = "org.freedesktop.NetworkManager"
)]
trait SettingsConnection {
    fn get_settings(&self) -> zbus::Result<HashMap<String, HashMap<String, OwnedValue>>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active(id: &str, kind: &str, state: u32, is_port: bool) -> ActiveConnection {
        // An object-path element may only hold [A-Za-z0-9_], so ids such as
        // "tun-port" are mapped before they become part of the path.
        let element = id.replace(|c: char| !c.is_ascii_alphanumeric() && c != '_', "_");
        let path = format!("{NM_PATH}/ActiveConnection/{element}");
        ActiveConnection {
            path: OwnedObjectPath::try_from(path).unwrap(),
            id: id.into(),
            kind: kind.into(),
            state,
            is_port,
        }
    }

    fn chosen(actives: Vec<ActiveConnection>) -> Option<String> {
        choose(actives).map(|a| a.id)
    }

    #[test]
    fn type_priority_beats_list_order() {
        let actives = vec![
            active("tun0", "tun", STATE_ACTIVATED, false),
            active("wg0", "wireguard", STATE_ACTIVATED, false),
            active("vpn0", "vpn", STATE_ACTIVATED, false),
        ];
        assert_eq!(chosen(actives).as_deref(), Some("vpn0"));

        let actives = vec![
            active("tun0", "tun", STATE_ACTIVATED, false),
            active("wg0", "wireguard", STATE_ACTIVATING, false),
        ];
        assert_eq!(chosen(actives).as_deref(), Some("wg0"));
    }

    #[test]
    fn list_order_within_a_priority() {
        let actives = vec![
            active("tap0", "tap", STATE_ACTIVATED, false),
            active("tun0", "tun", STATE_ACTIVATED, false),
        ];
        assert_eq!(chosen(actives).as_deref(), Some("tap0"));
    }

    #[test]
    fn ports_and_other_connections_do_not_qualify() {
        let actives = vec![
            active("lan", "802-3-ethernet", STATE_ACTIVATED, false),
            active("tun-port", "tun", STATE_ACTIVATED, true),
            active("vpn-down", "vpn", 3, false),
        ];
        assert_eq!(chosen(actives), None);

        let actives = vec![
            active("tun-port", "tun", STATE_ACTIVATED, true),
            active("tap0", "tap", STATE_ACTIVATED, false),
        ];
        assert_eq!(chosen(actives).as_deref(), Some("tap0"));
    }

    #[test]
    fn deactivating_connections_are_skipped() {
        let actives = vec![
            active("vpn-down", "vpn", 3, false),
            active("tun0", "tun", STATE_ACTIVATED, false),
        ];
        assert_eq!(chosen(actives).as_deref(), Some("tun0"));
    }

    fn profile(kind: &str, timestamp: u64, name: &'static str) -> (String, u64, &'static str) {
        (kind.into(), timestamp, name)
    }

    #[test]
    fn most_recent_profile_is_toggled() {
        let profiles = vec![
            profile("802-11-wireless", 900, "wifi"),
            profile("vpn", 100, "office"),
            profile("wireguard", 300, "wg-home"),
            profile("tun", 500, "tun0"),
        ];
        assert_eq!(most_recent(profiles), Some("wg-home"));
    }

    #[test]
    fn never_used_profiles_rank_last_and_ties_keep_order() {
        let profiles = vec![profile("wireguard", 0, "fresh"), profile("vpn", 0, "new")];
        assert_eq!(most_recent(profiles), Some("fresh"));

        let profiles = vec![
            profile("vpn", 0, "never"),
            profile("vpn", 7, "first"),
            profile("wireguard", 7, "second"),
        ];
        assert_eq!(most_recent(profiles), Some("first"));

        assert_eq!(most_recent(vec![profile("tun", 7, "tun0")]), None);
    }

    fn properties_changed(
        interface: &str,
        changed: &[&str],
        invalidated: &[&str],
    ) -> zbus::Message {
        let changed: HashMap<&str, Value<'_>> = changed
            .iter()
            .map(|key| (*key, Value::from(1u32)))
            .collect();
        zbus::Message::signal(
            NM_PATH,
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
        )
        .unwrap()
        .build(&(interface, changed, invalidated.to_vec()))
        .unwrap()
    }

    #[test]
    fn only_watched_properties_are_relevant() {
        assert!(is_relevant(&properties_changed(
            MANAGER_INTERFACE,
            &["ActiveConnections", "Connectivity"],
            &[]
        )));
        assert!(!is_relevant(&properties_changed(
            MANAGER_INTERFACE,
            &["Connectivity", "PrimaryConnection"],
            &[]
        )));
        assert!(is_relevant(&properties_changed(
            ACTIVE_INTERFACE,
            &[],
            &["Controller"]
        )));
        assert!(!is_relevant(&properties_changed(
            ACTIVE_INTERFACE,
            &["Ip4Config"],
            &[]
        )));
        assert!(!is_relevant(&properties_changed(
            "org.freedesktop.NetworkManager.Device",
            &["State"],
            &[]
        )));
    }
}
