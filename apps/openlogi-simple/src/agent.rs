//! The link to the background agent.
//!
//! The agent owns every device and the input hook; this app is a pure IPC
//! client of it, as the upstream GUI is. [`Agent`] owns a small tokio runtime
//! on its own thread and exposes the handful of calls the page needs as
//! blocking methods, so the AppKit main thread never touches async code.
//!
//! **Call these from a worker thread, never the main thread.** Each call can
//! wait on the agent, up to the RPC deadline. [`Agent`] is cheap to clone, so a
//! clone moves into a worker closure.
//!
//! Connecting follows the upstream GUI's policy, which `openlogi-ipc` owns:
//! [`connect_as`] with [`ClientKind::Gui`] reaches the socket, verifies the
//! protocol, and declares the client (which arms a dormant agent). A failure is
//! sorted into an [`AgentState`]: not running, or version skew, where only a
//! newer agent than this build means a relaunch of this app is needed.
//! [`Agent::connect_or_launch`] starts the agent the way upstream does, through
//! [`crate::launch::spawn_agent`].
//!
//! The write calls mirror the upstream GUI's request types
//! (`services/ipc/request.rs`): `set_dpi` and `set_smartshift` apply a value to
//! the device now, and [`Agent::reload_saved_config`] is
//! `reload_saved_config_with_verdict`, the call the GUI makes after every save.
//! A failed transport call drops the link, so the next call reports
//! [`AgentState::NotRunning`] until [`Agent::connect`] succeeds again.

use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use openlogi_core::config::{Config, ConfigRevision};
use openlogi_core::device::DeviceInventory;
use openlogi_core::hid::{DeviceRoute, Dpi, SmartShiftStatus, WriteError};
use openlogi_ipc::client::{ConnectError, ProtocolSkew, connect_as};
use openlogi_ipc::{
    AgentClient, AgentSnapshot, ClientKind, ConfigReloadError, InventoryHealth, SavedConfig,
    SavedConfigVerdict,
};
use tarpc::client::RpcError;
use tarpc::context;
use tokio::runtime::{Builder, Runtime};

use crate::devices::Device;
use crate::launch;
use crate::model::{Facts, Reading};

/// How often [`Agent::connect_or_launch`] retries while the agent starts, as
/// the upstream GUI's spawn reflex does.
const RETRY_EVERY: Duration = Duration::from_millis(250);

/// Whether this app can talk to the agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentState {
    /// Connected and version-matched.
    Connected,
    /// The socket could not be reached, or the agent never finished the
    /// handshake: not running, or still starting.
    NotRunning,
    /// The agent speaks another protocol. Writes are refused. An older agent is
    /// waiting to be replaced; a newer one means this app is stale and needs a
    /// relaunch ([`ProtocolSkew::AgentNewer`]).
    VersionSkew(ProtocolSkew),
}

/// Why an agent call did not produce its answer.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// There is no connection to call over; see [`Agent::state`].
    #[error("the agent is not available ({0:?})")]
    Unavailable(AgentState),
    /// The connection broke during the call. The link is down until the next
    /// successful [`Agent::connect`].
    #[error("lost the connection to the agent: {0}")]
    Lost(#[from] RpcError),
    /// The device refused or could not take the write.
    #[error("the device did not take it: {0}")]
    Device(WriteError),
    /// The agent could not adopt the saved config.
    #[error("the agent rejected the saved config: {}", .0.message)]
    ConfigRejected(ConfigReloadError),
    /// The agent has no authoritative device list yet.
    #[error("the agent has not finished scanning for devices ({0:?})")]
    Inventory(InventoryHealth),
}

struct Link {
    client: Option<AgentClient>,
    state: AgentState,
}

struct Inner {
    runtime: Runtime,
    link: Mutex<Link>,
}

/// The agent connection. Clones share one connection and one runtime.
#[derive(Clone)]
pub struct Agent {
    inner: Arc<Inner>,
}

impl Agent {
    /// A link that is not connected yet, with its runtime thread started.
    ///
    /// # Errors
    ///
    /// The runtime thread could not be started.
    pub fn new() -> std::io::Result<Self> {
        let runtime = Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("openlogi-agent-link")
            .enable_all()
            .build()?;
        Ok(Self {
            inner: Arc::new(Inner {
                runtime,
                link: Mutex::new(Link {
                    client: None,
                    state: AgentState::NotRunning,
                }),
            }),
        })
    }

    /// The state as of the last connect or failed call.
    #[must_use]
    pub fn state(&self) -> AgentState {
        self.link().state
    }

    /// One connection attempt, replacing any existing connection. Never starts
    /// the agent.
    #[must_use]
    pub fn connect(&self) -> AgentState {
        let outcome = self.inner.runtime.block_on(connect_as(ClientKind::Gui));
        let mut link = self.link();
        match outcome {
            Ok(client) => {
                link.client = Some(client);
                link.state = AgentState::Connected;
            }
            Err(error) => {
                link.client = None;
                link.state = state_for(&error);
            }
        }
        link.state
    }

    /// Connect, and when the agent is not there start it once the way the
    /// upstream GUI does ([`launch::spawn_agent`]: kickstart the registered
    /// service, registering it on demand), then retry until `wait` elapses.
    ///
    /// A newer agent is never launched over: starting cannot help, only a
    /// relaunch of this app does. Registering the service is a change to the
    /// user's login items, so only call this where starting the agent is the
    /// intent.
    #[must_use]
    pub fn connect_or_launch(&self, wait: Duration) -> AgentState {
        let deadline = Instant::now() + wait;
        let mut launched = false;
        loop {
            let state = self.connect();
            let settled = matches!(
                state,
                AgentState::Connected | AgentState::VersionSkew(ProtocolSkew::AgentNewer { .. })
            );
            if settled || Instant::now() >= deadline {
                return state;
            }
            if !launched {
                launch::spawn_agent();
                launched = true;
            }
            std::thread::sleep(RETRY_EVERY);
        }
    }

    /// Status and inventory together, as one poll result.
    ///
    /// # Errors
    ///
    /// [`AgentError::Unavailable`] without a connection, [`AgentError::Lost`]
    /// if it breaks.
    pub fn snapshot(&self) -> Result<AgentSnapshot, AgentError> {
        self.call(|client| async move { client.snapshot(context::current()).await })
    }

    /// The agent's receiver inventory, once its first scan has completed. An
    /// empty list then means no devices; before that it would be ambiguous,
    /// which is why a scan in progress is an error.
    ///
    /// # Errors
    ///
    /// [`AgentError::Inventory`] while scanning or when enumeration is broken,
    /// otherwise as [`Agent::snapshot`].
    pub fn inventory(&self) -> Result<Vec<DeviceInventory>, AgentError> {
        let snapshot = self.snapshot()?;
        match snapshot.status.inventory {
            InventoryHealth::Ready => Ok(snapshot.inventory),
            health @ (InventoryHealth::Scanning | InventoryHealth::Unavailable) => {
                Err(AgentError::Inventory(health))
            }
        }
    }

    /// The mice the agent reports, resolved against `config`.
    ///
    /// # Errors
    ///
    /// As [`Agent::inventory`].
    pub fn devices(&self, config: &Config) -> Result<Vec<Device>, AgentError> {
        Ok(Device::list(config, &self.inventory()?))
    }

    /// The page facts for `device`: its route and capabilities, plus the live
    /// DPI and SmartShift read from the device when it is online and routable.
    /// A failed read is [`Reading::Unavailable`], a device without the feature
    /// [`Reading::Unsupported`], as the upstream read service sorts them.
    #[must_use]
    pub fn facts(&self, device: &Device) -> Facts {
        let mut facts = device.facts();
        let Some(route) = device.route.as_ref().filter(|_| device.online) else {
            return facts;
        };
        let dpi_route = route.clone();
        facts.dpi = reading(
            self.call(|client| async move { client.read_dpi(context::current(), dpi_route).await }),
            |error| {
                matches!(
                    error,
                    WriteError::FeatureUnsupported { .. } | WriteError::EmptyDpiList
                )
            },
        );
        let smartshift_route = route.clone();
        facts.smartshift = reading(
            self.call(|client| async move {
                client
                    .read_smartshift(context::current(), smartshift_route)
                    .await
            }),
            |error| matches!(error, WriteError::FeatureUnsupported { .. }),
        );
        facts
    }

    /// Apply `dpi` to the device on `route` now. Save it only after this
    /// returns `Ok`: the agent writes the saved DPI again on every reconnect,
    /// so a value the device refused must never reach the file.
    ///
    /// # Errors
    ///
    /// [`AgentError::Device`] with the device's refusal (for example
    /// [`WriteError::DpiNotApplied`], which names the DPI it kept), otherwise
    /// as [`Agent::snapshot`].
    pub fn set_dpi(&self, route: &DeviceRoute, dpi: Dpi) -> Result<(), AgentError> {
        let route = route.clone();
        self.call(|client| async move { client.set_dpi(context::current(), route, dpi).await })?
            .map_err(AgentError::Device)
    }

    /// Apply the whole SmartShift `status` to the device on `route` now.
    ///
    /// # Errors
    ///
    /// As [`Agent::set_dpi`].
    pub fn set_smartshift(
        &self,
        route: &DeviceRoute,
        status: SmartShiftStatus,
    ) -> Result<(), AgentError> {
        let route = route.clone();
        self.call(|client| async move {
            client
                .set_smartshift(context::current(), route, status)
                .await
        })?
        .map_err(AgentError::Device)
    }

    /// Tell the agent the config was saved at `revision`, so it rebuilds its
    /// live maps from the file. [`SavedConfigVerdict::FileChangedAfterSave`]
    /// means another writer replaced the file after this save and the agent
    /// applied that instead; the caller should [`crate::store::Store::refresh`].
    ///
    /// # Errors
    ///
    /// [`AgentError::ConfigRejected`] when the agent cannot adopt the file,
    /// otherwise as [`Agent::snapshot`].
    pub fn reload_saved_config(
        &self,
        revision: ConfigRevision,
    ) -> Result<SavedConfigVerdict, AgentError> {
        let saved = SavedConfig {
            revision,
            chosen: Vec::new(),
        };
        self.call(|client| async move {
            client
                .reload_saved_config_with_verdict(context::current(), saved)
                .await
        })?
        .map_err(AgentError::ConfigRejected)
    }

    /// Have the agent re-read a `config.toml` it did not see this app save,
    /// as the upstream GUI does once at startup so edits made while it was
    /// closed apply.
    ///
    /// # Errors
    ///
    /// As [`Agent::reload_saved_config`].
    pub fn reload_config(&self) -> Result<(), AgentError> {
        self.call(|client| async move { client.reload_config(context::current()).await })?
            .map_err(AgentError::ConfigRejected)
    }

    fn link(&self) -> MutexGuard<'_, Link> {
        self.inner
            .link
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Run one RPC on the runtime. A broken transport drops the link, so the
    /// state never claims a connection that has gone.
    fn call<T, Fut>(&self, rpc: impl FnOnce(AgentClient) -> Fut) -> Result<T, AgentError>
    where
        Fut: Future<Output = Result<T, RpcError>>,
    {
        let client = {
            let link = self.link();
            link.client
                .clone()
                .ok_or(AgentError::Unavailable(link.state))?
        };
        self.inner.runtime.block_on(rpc(client)).map_err(|error| {
            let mut link = self.link();
            link.client = None;
            link.state = AgentState::NotRunning;
            AgentError::Lost(error)
        })
    }
}

/// Sort a failed connect into what the window should say.
fn state_for(error: &ConnectError) -> AgentState {
    match error {
        ConnectError::Skew(skew) => AgentState::VersionSkew(*skew),
        ConnectError::Endpoint(_) | ConnectError::Handshake(_) | ConnectError::Timeout => {
            AgentState::NotRunning
        }
    }
}

/// A device read as a [`Reading`]: its value, `Unsupported` for an error
/// `permanent` calls final, otherwise `Unavailable`.
fn reading<T>(
    answer: Result<Result<T, WriteError>, AgentError>,
    permanent: fn(&WriteError) -> bool,
) -> Reading<T> {
    match answer {
        Ok(Ok(value)) => Reading::Ready(value),
        Ok(Err(error)) if permanent(&error) => Reading::Unsupported,
        Ok(Err(_)) | Err(_) => Reading::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use openlogi_core::config::Config;
    use openlogi_core::device::{DeviceKind, ReceiverInfo};
    use openlogi_core::hid::{DpiCapabilities, DpiInfo, SmartShiftAutoDisengage, SmartShiftMode};
    use openlogi_ipc::testing::{Answer, in_memory_agent};
    use openlogi_ipc::{AgentRequest, AgentResponse};
    use tokio::sync::oneshot;

    use super::*;

    fn route() -> DeviceRoute {
        DeviceRoute::Bolt {
            receiver_uid: "r1".to_owned(),
            slot: 1,
        }
    }

    fn dpi_info() -> DpiInfo {
        DpiInfo {
            current: Dpi::new(1600),
            capabilities: DpiCapabilities::new(vec![400, 800, 1600]).expect("a list"),
        }
    }

    fn smartshift() -> SmartShiftStatus {
        SmartShiftStatus {
            mode: SmartShiftMode::Ratchet,
            auto_disengage: SmartShiftAutoDisengage::Permanent,
            tunable_torque: None,
        }
    }

    fn device(online: bool, route: Option<DeviceRoute>) -> Device {
        Device {
            key: "unit:aa".to_owned(),
            persistent: true,
            name: "MX".to_owned(),
            route,
            route_key: "bolt:r1:1".to_owned(),
            online,
            battery: None,
            capabilities: None,
        }
    }

    /// An agent over an in-memory server answering with `handler`; `stop`
    /// ends the server, as the agent exiting does.
    fn scripted(
        handler: impl Fn(AgentRequest) -> Answer + Clone + Send + Sync + 'static,
    ) -> (Agent, oneshot::Sender<()>) {
        let agent = Agent::new().expect("a runtime");
        let (stop, stopped) = oneshot::channel::<()>();
        let client = agent.inner.runtime.block_on(async {
            in_memory_agent(handler, async move {
                let _ = stopped.await;
            })
        });
        {
            let mut link = agent.link();
            link.client = Some(client);
            link.state = AgentState::Connected;
        }
        (agent, stop)
    }

    fn ok(response: AgentResponse) -> Answer {
        Box::pin(async move { Ok(response) })
    }

    #[test]
    fn a_connect_failure_is_sorted_by_who_is_stale() {
        let io = std::io::Error::from(std::io::ErrorKind::ConnectionRefused);
        assert_eq!(
            state_for(&ConnectError::Endpoint(io)),
            AgentState::NotRunning
        );
        assert_eq!(state_for(&ConnectError::Timeout), AgentState::NotRunning);
        let newer = ProtocolSkew::AgentNewer { agent: u32::MAX };
        assert_eq!(
            state_for(&ConnectError::Skew(newer)),
            AgentState::VersionSkew(newer)
        );
    }

    #[test]
    fn an_unconnected_link_refuses_every_call() {
        let agent = Agent::new().expect("a runtime");
        assert_eq!(agent.state(), AgentState::NotRunning);
        assert!(matches!(
            agent.snapshot(),
            Err(AgentError::Unavailable(AgentState::NotRunning))
        ));
        assert!(matches!(
            agent.set_dpi(&route(), Dpi::new(800)),
            Err(AgentError::Unavailable(_))
        ));
    }

    #[test]
    fn writes_carry_the_route_and_value_and_surface_the_devices_refusal() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        let (agent, _stop) = scripted(move |request| {
            log.lock().expect("log").push(format!("{request:?}"));
            match request {
                AgentRequest::SetDpi { dpi, .. } if dpi == Dpi::new(800) => {
                    ok(AgentResponse::SetDpi(Ok(())))
                }
                AgentRequest::SetDpi { dpi, .. } => {
                    ok(AgentResponse::SetDpi(Err(WriteError::DpiNotApplied {
                        requested: dpi,
                        actual: Dpi::new(1600),
                    })))
                }
                AgentRequest::SetSmartshift { .. } => ok(AgentResponse::SetSmartshift(Ok(()))),
                other => panic!("unexpected {other:?}"),
            }
        });

        agent
            .set_dpi(&route(), Dpi::new(800))
            .expect("the device took it");
        let refused = agent.set_dpi(&route(), Dpi::new(1200));
        assert!(
            matches!(
                refused,
                Err(AgentError::Device(WriteError::DpiNotApplied { .. }))
            ),
            "{refused:?}"
        );
        agent
            .set_smartshift(&route(), smartshift())
            .expect("applied");

        let seen = seen.lock().expect("log").clone();
        assert_eq!(seen.len(), 3);
        assert!(seen[0].contains("SetDpi") && seen[0].contains("800") && seen[0].contains("r1"));
        assert!(seen[2].contains("SetSmartshift") && seen[2].contains("Permanent"));
        assert_eq!(
            agent.state(),
            AgentState::Connected,
            "a refusal is not a lost link"
        );
    }

    #[test]
    fn reload_names_the_saved_revision_and_reports_the_verdict() {
        let revision = ConfigRevision::of("saved");
        let (agent, _stop) = scripted(move |request| match request {
            AgentRequest::ReloadSavedConfigWithVerdict { saved } => {
                assert_eq!(saved.revision, revision);
                assert!(saved.chosen.is_empty());
                ok(AgentResponse::ReloadSavedConfigWithVerdict(Ok(
                    SavedConfigVerdict::FileChangedAfterSave,
                )))
            }
            AgentRequest::ReloadConfig {} => {
                ok(AgentResponse::ReloadConfig(Err(ConfigReloadError {
                    message: "bad toml".to_owned(),
                })))
            }
            other => panic!("unexpected {other:?}"),
        });

        assert_eq!(
            agent.reload_saved_config(revision).expect("answers"),
            SavedConfigVerdict::FileChangedAfterSave
        );
        let rejected = agent.reload_config();
        assert!(
            matches!(&rejected, Err(AgentError::ConfigRejected(error)) if error.message == "bad toml"),
            "{rejected:?}"
        );
    }

    fn mouse_inventory(health: InventoryHealth) -> AgentResponse {
        use openlogi_core::device::{DeviceInventory, PairedDevice as Paired};
        let paired = Paired {
            slot: 1,
            codename: Some("MX Master 3S".to_owned()),
            wpid: Some(0xb034),
            kind: DeviceKind::Mouse,
            online: true,
            battery: None,
            model_info: None,
            capabilities: None,
            settings_revision: 0,
        };
        let snapshot = AgentSnapshot {
            status: openlogi_ipc::AgentStatus {
                accessibility_granted: true,
                hook_installed: true,
                launch_at_login: true,
                inventory: health,
                protocol_version: openlogi_ipc::PROTOCOL_VERSION,
                agent_version: "test".to_owned(),
                input_monitoring_granted: true,
                hid_open_failures: false,
                secure_input: openlogi_ipc::SecureInput::Off,
                virtual_keyboard: openlogi_ipc::VirtualKeyboardStatus::Unsupported,
            },
            inventory: vec![DeviceInventory {
                receiver: ReceiverInfo {
                    name: "Logi Bolt Receiver".to_owned(),
                    vendor_id: 0x046d,
                    product_id: 0xc548,
                    unique_id: Some("R1".to_owned()),
                },
                paired: if health == InventoryHealth::Ready {
                    vec![paired]
                } else {
                    Vec::new()
                },
            }],
            standalone: Vec::new(),
            camera_active: false,
            pairing: None,
            foreground: openlogi_ipc::ForegroundApps::default(),
            lights: Vec::new(),
        };
        AgentResponse::Snapshot(snapshot)
    }

    #[test]
    fn a_scanning_agent_has_no_inventory_yet() {
        let (agent, _stop) = scripted(|request| match request {
            AgentRequest::Snapshot {} => ok(mouse_inventory(InventoryHealth::Scanning)),
            other => panic!("unexpected {other:?}"),
        });
        assert!(matches!(
            agent.inventory(),
            Err(AgentError::Inventory(InventoryHealth::Scanning))
        ));

        let (agent, _stop) = scripted(|request| match request {
            AgentRequest::Snapshot {} => ok(mouse_inventory(InventoryHealth::Ready)),
            other => panic!("unexpected {other:?}"),
        });
        let devices = agent
            .devices(&Config::default())
            .expect("a ready inventory lists");
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "MX Master 3S");
    }

    #[test]
    fn facts_read_the_live_dpi_and_smartshift_of_an_online_device() {
        let (agent, _stop) = scripted(|request| match request {
            AgentRequest::ReadDpi { .. } => ok(AgentResponse::ReadDpi(Ok(dpi_info()))),
            AgentRequest::ReadSmartshift { .. } => {
                ok(AgentResponse::ReadSmartshift(Ok(smartshift())))
            }
            other => panic!("unexpected {other:?}"),
        });
        let facts = agent.facts(&device(true, Some(route())));
        assert_eq!(facts.dpi, Reading::Ready(dpi_info()));
        assert_eq!(facts.smartshift, Reading::Ready(smartshift()));
        assert_eq!(facts.route_key.as_deref(), Some("bolt:r1:1"));
    }

    #[test]
    fn facts_tell_a_missing_feature_from_a_failed_read() {
        let (agent, _stop) = scripted(|request| match request {
            AgentRequest::ReadDpi { .. } => ok(AgentResponse::ReadDpi(Err(
                WriteError::FeatureUnsupported {
                    feature_hex: 0x2201,
                },
            ))),
            AgentRequest::ReadSmartshift { .. } => ok(AgentResponse::ReadSmartshift(Err(
                WriteError::DeviceUnreachable { index: 1 },
            ))),
            other => panic!("unexpected {other:?}"),
        });
        let facts = agent.facts(&device(true, Some(route())));
        assert_eq!(facts.dpi, Reading::Unsupported);
        assert_eq!(facts.smartshift, Reading::Unavailable);
    }

    #[test]
    fn facts_ask_nothing_of_an_offline_or_unroutable_device() {
        let (agent, _stop) = scripted(|request| panic!("a read was sent: {request:?}"));
        for device in [device(false, Some(route())), device(true, None)] {
            let facts = agent.facts(&device);
            assert_eq!(facts.dpi, Reading::Unavailable);
            assert_eq!(facts.smartshift, Reading::Unavailable);
        }
    }

    #[test]
    fn a_broken_transport_drops_the_link_until_the_next_connect() {
        let (agent, stop) = scripted(|request| match request {
            AgentRequest::Snapshot {} => ok(mouse_inventory(InventoryHealth::Ready)),
            other => panic!("unexpected {other:?}"),
        });
        agent.snapshot().expect("the first call answers");
        let _ = stop.send(());

        let mut lost = None;
        for _ in 0..40 {
            if let Err(error) = agent.snapshot() {
                lost = Some(error);
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(matches!(lost, Some(AgentError::Lost(_))), "{lost:?}");
        assert_eq!(agent.state(), AgentState::NotRunning);
        assert!(matches!(
            agent.snapshot(),
            Err(AgentError::Unavailable(AgentState::NotRunning))
        ));
    }
}
