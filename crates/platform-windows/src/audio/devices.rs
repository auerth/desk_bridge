//! Endpoint enumeration, hot-plug watching and endpoint volume inspection.

use super::util::{StateTransition, classify_state, looks_virtual};
use super::wasapi::{
    ComGuard, data_flow, device_id, device_kind, enumerator, open_device, os_ctx, os_err,
};
use deskbridge_platform::audio::{AudioDeviceInfo, AudioDevices, DeviceEvent, DeviceKind};
use deskbridge_platform::{PlatformError, Result, UnboundedSender};
use parking_lot::Mutex;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    DEVICE_STATE, DEVICE_STATE_ACTIVE, EDataFlow, ERole, IMMDevice, IMMDeviceEnumerator,
    IMMNotificationClient, IMMNotificationClient_Impl, eCapture, eCommunications, eConsole,
    eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, STGM_READ};
use windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY;
use windows::core::{BSTR, GUID, PCWSTR, implement};

/// `PKEY_Device_DeviceDesc`: for a headset dongle endpoint this is the short role name ("Game",
/// "Chat"), while the friendly name is "Game (Vendor Dongle)".
const PKEY_DEVICE_DESC: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
    pid: 2,
};
/// `PKEY_DeviceInterface_FriendlyName`: the adapter / dongle name ("Realtek(R) Audio").
const PKEY_DEVICE_INTERFACE_FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0x026e516e_b814_414b_83cd_856d6fef4822),
    pid: 2,
};
/// `PKEY_Device_EnumeratorName`: PnP enumerator ("HDAUDIO", "USB", "BTHENUM", "ROOT", ...).
const PKEY_DEVICE_ENUMERATOR_NAME: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
    pid: 24,
};

/// [`AudioDeviceInfo`] plus the raw pieces of the friendly name, so a UI can disambiguate
/// several endpoints of one dongle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceDetails {
    pub info: AudioDeviceInfo,
    /// Short endpoint description, e.g. "Game" / "Chat" / "Speakers".
    pub description: Option<String>,
    /// Name of the hosting adapter / dongle, e.g. "Arctis Nova 7 Wireless".
    pub adapter: Option<String>,
    /// PnP enumerator (`USB`, `HDAUDIO`, `BTHENUM`, `ROOT`, ...).
    pub enumerator: Option<String>,
}

fn prop_string(dev: &IMMDevice, key: &PROPERTYKEY) -> Option<String> {
    // SAFETY: property store and value are owned COM wrappers released on drop; the key outlives
    // the call.
    unsafe {
        let store = dev.OpenPropertyStore(STGM_READ).ok()?;
        let pv = store.GetValue(key).ok()?;
        let s = BSTR::try_from(&pv).ok()?.to_string();
        (!s.is_empty()).then_some(s)
    }
}

/// Friendly name of an endpoint (falls back to its id).
pub(super) fn friendly_name(dev: &IMMDevice) -> String {
    prop_string(dev, &PKEY_Device_FriendlyName)
        .or_else(|| device_id(dev).ok())
        .unwrap_or_default()
}

fn default_id(en: &IMMDeviceEnumerator, kind: DeviceKind, role: ERole) -> Option<String> {
    // SAFETY: simple getter.
    let dev = unsafe { en.GetDefaultAudioEndpoint(data_flow(kind), role) }.ok()?;
    device_id(&dev).ok()
}

fn describe(
    dev: &IMMDevice,
    kind: DeviceKind,
    default: Option<&str>,
    default_comm: Option<&str>,
) -> windows::core::Result<DeviceDetails> {
    let id = device_id(dev)?;
    let name = prop_string(dev, &PKEY_Device_FriendlyName).unwrap_or_else(|| id.clone());
    let description = prop_string(dev, &PKEY_DEVICE_DESC);
    let adapter = prop_string(dev, &PKEY_DEVICE_INTERFACE_FRIENDLY_NAME);
    let enumerator = prop_string(dev, &PKEY_DEVICE_ENUMERATOR_NAME);
    let is_virtual = looks_virtual(&name, description.as_deref(), enumerator.as_deref());
    Ok(DeviceDetails {
        info: AudioDeviceInfo {
            is_default: default == Some(id.as_str()),
            is_default_communications: default_comm == Some(id.as_str()),
            id,
            name,
            kind,
            is_virtual,
        },
        description,
        adapter,
        enumerator,
    })
}

fn list_with(en: &IMMDeviceEnumerator, kind: DeviceKind) -> Result<Vec<DeviceDetails>> {
    let def = default_id(en, kind, eConsole);
    let comm = default_id(en, kind, eCommunications);
    // SAFETY: standard enumeration; the collection and devices are released on drop.
    unsafe {
        let coll = en
            .EnumAudioEndpoints(data_flow(kind), DEVICE_STATE_ACTIVE)
            .map_err(os_err)?;
        let n = coll.GetCount().map_err(os_err)?;
        let mut out = Vec::with_capacity(n as usize);
        for i in 0..n {
            // Devices can vanish between GetCount and Item; skip those.
            let Ok(dev) = coll.Item(i) else { continue };
            match describe(&dev, kind, def.as_deref(), comm.as_deref()) {
                Ok(d) => out.push(d),
                Err(e) => tracing::debug!("skipping audio endpoint {i}: {e}"),
            }
        }
        Ok(out)
    }
}

/// Snapshot of an endpoint's volume / mute state (diagnostics, e.g. loopback-with-mute probing).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EndpointVolume {
    pub muted: bool,
    /// Master volume scalar 0.0..=1.0.
    pub scalar: f32,
}

/// Reads mute and master volume of a render or capture endpoint (`None` = default render). Never
/// modifies anything.
pub fn endpoint_volume(kind: DeviceKind, device_id: Option<&str>) -> Result<EndpointVolume> {
    let _com = ComGuard::init();
    let en = enumerator().map_err(os_err)?;
    let role = if kind == DeviceKind::Capture {
        eCommunications
    } else {
        eConsole
    };
    let dev = open_device(&en, kind, device_id, role)?;
    // SAFETY: activation returns an owned interface; the getters only read.
    unsafe {
        let vol: IAudioEndpointVolume = dev
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| os_ctx("Activate(IAudioEndpointVolume)", e))?;
        Ok(EndpointVolume {
            muted: vol.GetMute().map_err(os_err)?.as_bool(),
            scalar: vol.GetMasterVolumeLevelScalar().map_err(os_err)?,
        })
    }
}

/// Mutes/unmutes a render endpoint (`None` = default render device). Returns the endpoint id and
/// the previous mute state.
pub fn set_render_mute(device_id: Option<&str>, mute: bool) -> Result<(String, bool)> {
    let _com = ComGuard::init();
    let en = enumerator().map_err(os_err)?;
    let dev = open_device(&en, DeviceKind::Render, device_id, eConsole)?;
    let id = device_id_of(&dev)?;
    // SAFETY: activation returns an owned interface; GetMute/SetMute are plain calls.
    unsafe {
        let vol: IAudioEndpointVolume = dev
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| os_ctx("Activate(IAudioEndpointVolume)", e))?;
        let prev = vol.GetMute().map_err(os_err)?.as_bool();
        vol.SetMute(mute, std::ptr::null()).map_err(os_err)?;
        Ok((id, prev))
    }
}

fn device_id_of(dev: &IMMDevice) -> Result<String> {
    device_id(dev).map_err(os_err)
}

/// [`LocalOutput`](deskbridge_platform::audio::LocalOutput) over `IAudioEndpointVolume`.
#[derive(Debug, Default, Clone, Copy)]
pub struct WinLocalOutput;

impl WinLocalOutput {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl deskbridge_platform::audio::LocalOutput for WinLocalOutput {
    fn set_default_render_mute(&self, mute: bool) -> Result<(String, bool)> {
        set_render_mute(None, mute)
    }

    fn set_render_mute(&self, device_id: &str, mute: bool) -> Result<()> {
        set_render_mute(Some(device_id), mute).map(|_| ())
    }
}

// ---------------------------------------------------------------------------------------------
// Hot-plug notifications
// ---------------------------------------------------------------------------------------------

/// Raw notification forwarded from the COM callback thread to the watcher thread. The callback
/// itself does no COM calls (they can deadlock inside notification callbacks).
#[derive(Debug)]
enum Raw {
    State {
        id: String,
        state: u32,
    },
    Added {
        id: String,
    },
    Removed {
        id: String,
    },
    Default {
        flow: EDataFlow,
        role: ERole,
        id: Option<String>,
    },
}

#[implement(IMMNotificationClient)]
struct NotifyClient {
    tx: mpsc::Sender<Raw>,
}

fn pcwstr_string(p: &PCWSTR) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: the pointer is a NUL-terminated string valid for the duration of the callback.
    unsafe { p.to_string().ok() }
}

#[allow(non_snake_case)]
impl IMMNotificationClient_Impl for NotifyClient_Impl {
    fn OnDeviceStateChanged(
        &self,
        pwstrdeviceid: &PCWSTR,
        dwnewstate: DEVICE_STATE,
    ) -> windows::core::Result<()> {
        if let Some(id) = pcwstr_string(pwstrdeviceid) {
            let _ = self.tx.send(Raw::State {
                id,
                state: dwnewstate.0,
            });
        }
        Ok(())
    }

    fn OnDeviceAdded(&self, pwstrdeviceid: &PCWSTR) -> windows::core::Result<()> {
        if let Some(id) = pcwstr_string(pwstrdeviceid) {
            let _ = self.tx.send(Raw::Added { id });
        }
        Ok(())
    }

    fn OnDeviceRemoved(&self, pwstrdeviceid: &PCWSTR) -> windows::core::Result<()> {
        if let Some(id) = pcwstr_string(pwstrdeviceid) {
            let _ = self.tx.send(Raw::Removed { id });
        }
        Ok(())
    }

    fn OnDefaultDeviceChanged(
        &self,
        flow: EDataFlow,
        role: ERole,
        pwstrdefaultdeviceid: &PCWSTR,
    ) -> windows::core::Result<()> {
        let _ = self.tx.send(Raw::Default {
            flow,
            role,
            id: pcwstr_string(pwstrdefaultdeviceid),
        });
        Ok(())
    }

    fn OnPropertyValueChanged(
        &self,
        _pwstrdeviceid: &PCWSTR,
        _key: &PROPERTYKEY,
    ) -> windows::core::Result<()> {
        Ok(())
    }
}

/// Tracks which endpoints are currently active and turns raw notifications into
/// [`DeviceEvent`]s. Idempotent: duplicate OS notifications yield one event.
struct Tracker {
    en: IMMDeviceEnumerator,
    known: HashSet<String>,
}

impl Tracker {
    fn new(en: IMMDeviceEnumerator) -> Result<Self> {
        let mut known = HashSet::new();
        for kind in [DeviceKind::Render, DeviceKind::Capture] {
            for d in list_with(&en, kind)? {
                known.insert(d.info.id);
            }
        }
        Ok(Self { en, known })
    }

    fn resolve(&self, id: &str) -> Option<(IMMDevice, bool)> {
        let h = windows::core::HSTRING::from(id);
        // SAFETY: `h` outlives the calls.
        unsafe {
            let dev = self.en.GetDevice(PCWSTR(h.as_ptr())).ok()?;
            let active = dev.GetState().ok()? == DEVICE_STATE_ACTIVE;
            Some((dev, active))
        }
    }

    fn on_state(&mut self, id: String, new_state: u32) -> Option<DeviceEvent> {
        match classify_state(self.known.contains(&id), new_state) {
            StateTransition::BecameActive => {
                let (dev, active) = self.resolve(&id)?;
                if !active {
                    return None;
                }
                let kind = device_kind(&dev).ok()?;
                let def = default_id(&self.en, kind, eConsole);
                let comm = default_id(&self.en, kind, eCommunications);
                let d = describe(&dev, kind, def.as_deref(), comm.as_deref()).ok()?;
                self.known.insert(id);
                Some(DeviceEvent::Added(d.info))
            }
            StateTransition::BecameInactive => {
                self.known.remove(&id);
                Some(DeviceEvent::Removed { id })
            }
            StateTransition::NoChange => None,
        }
    }

    fn handle(&mut self, raw: Raw) -> Option<DeviceEvent> {
        match raw {
            Raw::State { id, state } => self.on_state(id, state),
            Raw::Added { id } => {
                // A newly installed device may already be active.
                let (_, active) = self.resolve(&id)?;
                self.on_state(id, u32::from(active))
            }
            Raw::Removed { id } => self
                .known
                .remove(&id)
                .then_some(DeviceEvent::Removed { id }),
            Raw::Default { flow, role, id } => {
                // One user action fires console + multimedia + communications; report the two
                // roles we distinguish and let consumers re-list for the flags.
                if role != eConsole && role != eCommunications {
                    return None;
                }
                let kind = if flow == eRender {
                    DeviceKind::Render
                } else if flow == eCapture {
                    DeviceKind::Capture
                } else {
                    return None;
                };
                Some(DeviceEvent::DefaultChanged { kind, id })
            }
        }
    }
}

struct Watcher {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn watcher_main(
    tx: UnboundedSender<DeviceEvent>,
    stop: Arc<AtomicBool>,
    ready: mpsc::Sender<Result<()>>,
) {
    let _com = ComGuard::init();
    let (raw_tx, raw_rx) = mpsc::channel::<Raw>();

    let setup = || -> Result<(Tracker, IMMNotificationClient)> {
        let en = enumerator().map_err(os_err)?;
        let tracker = Tracker::new(en.clone())?;
        let client: IMMNotificationClient = NotifyClient { tx: raw_tx }.into();
        // SAFETY: `client` stays alive until we unregister it below.
        unsafe { en.RegisterEndpointNotificationCallback(&client) }.map_err(os_err)?;
        Ok((tracker, client))
    };
    let (mut tracker, client) = match setup() {
        Ok(v) => {
            let _ = ready.send(Ok(()));
            v
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };

    while !stop.load(Ordering::Acquire) && !tx.is_closed() {
        match raw_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(raw) => {
                if let Some(ev) = tracker.handle(raw) {
                    tracing::debug!(?ev, "audio device event");
                    if tx.send(ev).is_err() {
                        break;
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    // SAFETY: unregister the client we registered on this same enumerator.
    let _ = unsafe { tracker.en.UnregisterEndpointNotificationCallback(&client) };
}

/// WASAPI/MMDevice based [`AudioDevices`]. Dropping it stops all watcher threads.
#[derive(Default)]
pub struct WasapiDevices {
    watchers: Mutex<Vec<Watcher>>,
}

impl WasapiDevices {
    pub fn new() -> Self {
        Self::default()
    }

    /// Like [`AudioDevices::list`] but with the raw name parts for UI disambiguation.
    pub fn list_detailed(&self, kind: DeviceKind) -> Result<Vec<DeviceDetails>> {
        let _com = ComGuard::init();
        let en = enumerator().map_err(os_err)?;
        list_with(&en, kind)
    }
}

impl AudioDevices for WasapiDevices {
    fn list(&self, kind: DeviceKind) -> Result<Vec<AudioDeviceInfo>> {
        Ok(self
            .list_detailed(kind)?
            .into_iter()
            .map(|d| d.info)
            .collect())
    }

    /// Starts a watcher thread that lives until `tx`'s receiver is dropped or `self` is dropped.
    /// Headset power-off yields `Removed`, power-on `Added` (device state transitions to/from
    /// ACTIVE). Events are idempotent.
    fn watch(&self, tx: UnboundedSender<DeviceEvent>) -> Result<()> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel();
        let stop2 = stop.clone();
        let join = std::thread::Builder::new()
            .name("db-audio-watch".into())
            .spawn(move || watcher_main(tx, stop2, ready_tx))
            .map_err(|e| PlatformError::other(format!("spawn audio watcher: {e}")))?;
        let w = Watcher {
            stop,
            join: Some(join),
        };
        match ready_rx.recv() {
            Ok(Ok(())) => {
                self.watchers.lock().push(w);
                Ok(())
            }
            Ok(Err(e)) => {
                // Dropping `w` joins the already finished thread.
                w.stop.store(true, Ordering::Release);
                Err(e)
            }
            Err(_) => Err(PlatformError::other(
                "audio watcher thread died during setup",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Device-dependent: tolerates machines without audio endpoints.
    #[test]
    fn list_devices_does_not_fail() {
        let d = WasapiDevices::new();
        for kind in [DeviceKind::Render, DeviceKind::Capture] {
            match d.list_detailed(kind) {
                Ok(v) => {
                    // At most one default per kind among active devices.
                    assert!(v.iter().filter(|d| d.info.is_default).count() <= 1);
                    assert!(
                        v.iter()
                            .all(|d| d.info.kind == kind && !d.info.id.is_empty())
                    );
                }
                Err(e) => eprintln!("skipping: cannot enumerate {kind:?}: {e}"),
            }
        }
    }

    #[test]
    fn watch_starts_and_stops() {
        let d = WasapiDevices::new();
        let (tx, mut rx) = deskbridge_platform::unbounded_channel();
        match d.watch(tx) {
            Ok(()) => {
                assert!(rx.try_recv().is_err()); // nothing happens in a quiet test run
                drop(d); // joins the watcher thread
            }
            Err(e) => eprintln!("skipping: watch unavailable: {e}"),
        }
    }
}
