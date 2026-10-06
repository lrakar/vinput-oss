//! Microphone capture via WASAPI in shared, event-driven mode.
#![allow(unsafe_code)]
#![expect(
    clippy::ref_as_ptr,
    clippy::inline_always,
    reason = "in the code the `implement` macro of the windows crate writes"
)]

use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::OnceLock;

use windows::Win32::Devices::FunctionDiscovery::{
    PKEY_Device_EnumeratorName, PKEY_Device_FriendlyName,
};
use windows::Win32::Foundation::{
    CloseHandle, HANDLE, PROPERTYKEY, RPC_E_CHANGED_MODE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, DEVICE_STATE,
    DEVICE_STATE_ACTIVE, EDataFlow, ERole, IAudioCaptureClient, IAudioClient, IMMDevice,
    IMMDeviceEnumerator, IMMNotificationClient, IMMNotificationClient_Impl, MMDeviceEnumerator,
    WAVE_FORMAT_PCM, WAVEFORMATEX, eCapture, eConsole,
};
#[cfg(test)]
use windows::Win32::Media::Audio::{AUDCLNT_STREAMFLAGS_LOOPBACK, eRender};
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToBSTR};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoIncrementMTAUsage, CoInitializeEx,
    CoTaskMemFree, CoUninitialize, STGM_READ,
};
use windows::Win32::System::Power::{
    DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, HPOWERNOTIFY, RegisterSuspendResumeNotification,
    UnregisterSuspendResumeNotification,
};
use windows::Win32::System::Threading::{CreateEventW, ResetEvent, WaitForSingleObject};
use windows::Win32::UI::WindowsAndMessaging::{DEVICE_NOTIFY_CALLBACK, PBT_APMRESUMEAUTOMATIC};
use windows::core::{HSTRING, PCWSTR, PWSTR, implement};

use super::armed::{Armed, ArmedSource, CaptureBackend, CaptureStream, Changed};
use super::{Microphone, SAMPLE_RATE_HZ, StopHandle};
use crate::{Error, Result, log};

/// Requested capture buffer length, in 100-nanosecond units: one second.
///
/// The buffer is only room for audio that has not been read yet. Capture, the
/// recognizer and typing share one thread, and whenever that thread is held
/// up for longer than the buffer (a program that is slow to take the typed
/// text, a busy PC), Windows drops what does not fit without saying so. A
/// tenth of a second, as it used to be, is quickly used up; one second is
/// longer than any hold-up short of a hang, and holds 32 kB.
///
/// It adds no delay. In shared mode Windows signals each device period
/// (10 ms) whatever the size of the buffer, and the delay through it is only
/// how far the reader is behind the writer (`IAudioClient::Initialize`,
/// Remarks). Measured on a loopback stream: 16 000 frames granted, the first
/// packet 9 ms after the start and one every 10 ms from then on.
const BUFFER_DURATION_HNS: i64 = 10_000_000;
/// How long to wait for the device to signal new audio before checking for a stop.
const WAIT_MS: u32 = 200;
/// Consecutive silent waits after which the device is considered dead (5 seconds).
/// A healthy shared-mode stream signals every period, even when the room is quiet.
const MAX_IDLE_WAITS: u32 = 25;
const BYTES_PER_SAMPLE: usize = size_of::<i16>();
/// Bytes per mono 16-bit frame, as the format structure wants it.
const BLOCK_ALIGN: u16 = 2;
#[expect(clippy::cast_possible_truncation)]
const FORMAT_TAG_PCM: u16 = WAVE_FORMAT_PCM as u16;

/// The microphone of this PC, kept ready for a while after each session.
pub type ArmedMicrophone = Armed<Wasapi>;

/// A microphone, delivering mono 16-bit PCM at
/// [`SAMPLE_RATE_HZ`](super::SAMPLE_RATE_HZ).
///
/// Windows converts from the device's native format, so there is no resampling
/// code here.
pub type WasapiSource = ArmedSource<WasapiStream>;

impl Armed<Wasapi> {
    /// The microphone Windows knows by `id`, as [`microphones`] lists it.
    /// An empty `id` means the default microphone, and so does one that is
    /// not connected. Nothing is opened until a session asks for it.
    #[must_use]
    pub fn new(id: &str) -> Self {
        Self::with(Wasapi::microphone(), id)
    }
}

impl ArmedSource<WasapiStream> {
    /// Opens the default microphone and starts capturing. Triggering `stop` from
    /// any thread makes the next `read` return `Ok(0)`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Audio`] if there is no microphone, access to it is denied,
    /// or the device cannot be started.
    pub fn open_default(stop: StopHandle) -> Result<Self> {
        Self::open("", stop)
    }

    /// Opens the microphone Windows knows by `id`, as [`microphones`] lists
    /// it, and starts capturing. An empty `id` means the default microphone,
    /// and so does one that is not connected right now, so that dictation
    /// still works while the chosen one is unplugged.
    ///
    /// Sessions of the resident process open an [`ArmedMicrophone`]
    /// instead, which may still have the stream of the session before.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Audio`] if there is no microphone, access to it is denied,
    /// or the device cannot be started.
    pub fn open(id: &str, stop: StopHandle) -> Result<Self> {
        let mut stream = Wasapi::microphone().open(id)?;
        stream.start()?;
        Ok(Self::unarmed(stream, stop))
    }
}

/// Which endpoint streams are opened on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Endpoint {
    /// A microphone.
    Microphone,
    /// What the default speakers are playing. Only the tests use it, to run
    /// a real stream without ever recording from a microphone.
    #[cfg(test)]
    Loopback,
}

/// The microphones of this PC, as a [`CaptureBackend`].
#[derive(Debug)]
pub struct Wasapi {
    endpoint: Endpoint,
}

impl Wasapi {
    /// The microphones.
    #[must_use]
    pub fn microphone() -> Self {
        Self {
            endpoint: Endpoint::Microphone,
        }
    }

    /// The device a stream for `id` is opened on: the chosen one if it is
    /// connected, otherwise the default one.
    ///
    /// # Safety
    ///
    /// COM must be available on the calling thread.
    unsafe fn device(
        &self,
        enumerator: &IMMDeviceEnumerator,
        id: &str,
        announce: bool,
    ) -> Result<IMMDevice> {
        let flow = match self.endpoint {
            Endpoint::Microphone => {
                // SAFETY: as the caller promises.
                if let Some(device) = unsafe { chosen_device(enumerator, id, announce) } {
                    return Ok(device);
                }
                eCapture
            }
            #[cfg(test)]
            Endpoint::Loopback => eRender,
        };
        // SAFETY: as the caller promises.
        unsafe { enumerator.GetDefaultAudioEndpoint(flow, eConsole) }
            .map_err(audio_error("finding the default microphone"))
    }
}

impl CaptureBackend for Wasapi {
    type Stream = WasapiStream;
    type Watch = WasapiWatch;

    fn open(&self, microphone: &str) -> Result<WasapiStream> {
        apartment()?;
        let format = WAVEFORMATEX {
            wFormatTag: FORMAT_TAG_PCM,
            nChannels: 1,
            nSamplesPerSec: SAMPLE_RATE_HZ,
            nAvgBytesPerSec: SAMPLE_RATE_HZ * u32::from(BLOCK_ALIGN),
            nBlockAlign: BLOCK_ALIGN,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        let flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK
            | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        #[cfg(test)]
        let flags = match self.endpoint {
            Endpoint::Microphone => flags,
            Endpoint::Loopback => flags | AUDCLNT_STREAMFLAGS_LOOPBACK,
        };
        // SAFETY: COM is available on this thread (`apartment`), and `format`
        // is a valid PCM format that outlives the `Initialize` call. Nothing
        // here starts the stream: Windows neither captures for it nor shows
        // the microphone as in use until `Start`.
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(audio_error("creating the device enumerator"))?;
            let device = self.device(&enumerator, microphone, true)?;
            let id = device_id(&device).ok_or_else(|| {
                Error::Audio("the microphone has no identifier in Windows".into())
            })?;
            let bluetooth = is_bluetooth(&device);
            let client: IAudioClient = device
                .Activate(CLSCTX_ALL, None)
                .map_err(audio_error("opening the microphone"))?;
            client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    flags,
                    BUFFER_DURATION_HNS,
                    0,
                    &raw const format,
                    None,
                )
                .map_err(audio_error("configuring the microphone"))?;
            let event = CreateEventW(None, false, false, None)
                .map_err(audio_error("creating the capture event"))?;
            let capture = client
                .SetEventHandle(event)
                .and_then(|()| client.GetService::<IAudioCaptureClient>());
            match capture {
                Ok(capture) => Ok(WasapiStream {
                    capture,
                    client,
                    event,
                    device: id,
                    bluetooth,
                    pending: VecDeque::new(),
                    running: false,
                }),
                Err(err) => {
                    let _ = CloseHandle(event);
                    Err(audio_error("setting up the microphone")(err))
                }
            }
        }
    }

    fn standby(&self, microphone: &str) -> Option<String> {
        apartment().ok()?;
        // SAFETY: COM is available on this thread (`apartment`).
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
            let device = self.device(&enumerator, microphone, false).ok()?;
            if is_bluetooth(&device) {
                return None;
            }
            device_id(&device)
        }
    }

    fn watch(&self, changed: Changed) -> Result<WasapiWatch> {
        let flow = match self.endpoint {
            Endpoint::Microphone => eCapture,
            #[cfg(test)]
            Endpoint::Loopback => eRender,
        };
        WasapiWatch::new(changed, flow)
    }
}

/// A stream from one microphone that is set up, and stopped unless a session
/// is capturing from it.
pub struct WasapiStream {
    capture: IAudioCaptureClient,
    client: IAudioClient,
    event: HANDLE,
    /// What Windows knows the device by.
    device: String,
    /// Whether the device is a Bluetooth one, which is never kept open.
    bluetooth: bool,
    /// Samples taken from the device that did not fit in the caller's buffer.
    pending: VecDeque<i16>,
    /// Whether the stream has been started and not stopped since.
    running: bool,
}

// SAFETY: the stream is made in the multithreaded apartment of the process,
// which `apartment` keeps alive for as long as the process runs. Objects of
// that apartment may be called from any thread in it, which every thread is
// that has not chosen another, and the stream is only ever handed to threads
// of our own that have not: the keeper of an armed microphone and session
// threads. It is owned, so one thread uses it at a time.
unsafe impl Send for WasapiStream {}

impl std::fmt::Debug for WasapiStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasapiStream")
            .field("device", &self.device)
            .finish_non_exhaustive()
    }
}

impl WasapiStream {
    /// Moves queued samples into `buf` and returns how many were written.
    fn drain_pending(&mut self, buf: &mut [i16]) -> usize {
        let count = buf.len().min(self.pending.len());
        for (slot, sample) in buf.iter_mut().zip(self.pending.drain(..count)) {
            *slot = sample;
        }
        count
    }

    /// Reads the next packet from the device into `pending`. Returns `false` if
    /// no packet is available yet.
    fn fetch_packet(&mut self) -> Result<bool> {
        // SAFETY: the capture client is live. `GetBuffer` yields `frames` mono 16-bit
        // frames at `data`, valid until the matching `ReleaseBuffer`; the bytes are
        // copied out before then and read without assuming alignment.
        unsafe {
            let available = self
                .capture
                .GetNextPacketSize()
                .map_err(audio_error("querying the microphone"))?;
            if available == 0 {
                return Ok(false);
            }
            let mut data = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            self.capture
                .GetBuffer(&raw mut data, &raw mut frames, &raw mut flags, None, None)
                .map_err(audio_error("reading from the microphone"))?;
            let samples = frames as usize;
            if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                self.pending.extend(std::iter::repeat_n(0, samples));
            } else {
                let bytes = std::slice::from_raw_parts(data, samples * BYTES_PER_SAMPLE);
                self.pending.extend(
                    bytes
                        .chunks_exact(BYTES_PER_SAMPLE)
                        .map(|b| i16::from_le_bytes([b[0], b[1]])),
                );
            }
            self.capture
                .ReleaseBuffer(frames)
                .map_err(audio_error("releasing the microphone buffer"))?;
        }
        Ok(true)
    }

    /// Throws away everything captured and not read: what this stream holds,
    /// what Windows holds for it, and a signal that was not waited for.
    ///
    /// # Safety
    ///
    /// The stream must be stopped.
    unsafe fn flush(&mut self) -> windows::core::Result<()> {
        self.pending.clear();
        // SAFETY: the client and the event are live, and the stream is
        // stopped, as `Reset` needs.
        unsafe {
            self.client.Reset()?;
            ResetEvent(self.event)
        }
    }
}

impl CaptureStream for WasapiStream {
    fn device(&self) -> &str {
        &self.device
    }

    fn may_stay_armed(&self) -> bool {
        // A Bluetooth headset has to leave its music mode for its telephone
        // mode to deliver a microphone, and may do so for as long as a
        // capture stream exists rather than only while one runs. Its stream
        // is opened for each session, as every stream used to be.
        !self.bluetooth
    }

    fn alive(&self) -> bool {
        // SAFETY: the client is live. The call fails once the device is gone.
        unsafe { self.client.GetCurrentPadding() }.is_ok()
    }

    fn start(&mut self) -> Result<()> {
        // SAFETY: the stream is stopped: it was opened stopped, and every
        // start is followed by `stop` before the next. Emptying it again
        // costs microseconds and makes sure that nothing of an earlier
        // session can be heard by this one.
        unsafe {
            self.flush()
                .and_then(|()| self.client.Start())
                .map_err(audio_error("starting the microphone"))?;
        }
        self.running = true;
        Ok(())
    }

    fn drain(&mut self, buf: &mut [i16]) -> Result<usize> {
        if self.running {
            self.running = false;
            // SAFETY: the client is live.
            unsafe { self.client.Stop() }.map_err(audio_error("stopping the microphone"))?;
        }
        // What Windows still holds was captured before the stop. With a
        // buffer of a second that can be the last second of what was said,
        // when the reader was busy typing. If Windows will not hand it over
        // from a stopped stream, the session ends with what it has.
        while self.pending.len() < buf.len() && self.fetch_packet().unwrap_or(false) {}
        Ok(self.drain_pending(buf))
    }

    fn stop(&mut self) -> Result<()> {
        self.running = false;
        // SAFETY: the client is live. Without `Reset`, Windows keeps what was
        // captured and not read and hands it to whoever starts the stream
        // next; it is thrown away here so that no audio outlives its session.
        unsafe {
            self.client
                .Stop()
                .and_then(|()| self.flush())
                .map_err(audio_error("stopping the microphone"))
        }
    }

    /// Hands over what the device has, as soon as it has anything: normally
    /// one packet of 10 ms, and more only when the reader has fallen behind.
    /// It never waits to fill `buf`, because audio that waits here is text
    /// that appears later.
    fn read(&mut self, buf: &mut [i16], stop: &StopHandle) -> Result<usize> {
        let mut idle_waits = 0;
        loop {
            if stop.is_stopped() {
                return Ok(0);
            }
            while self.pending.len() < buf.len() && self.fetch_packet()? {}
            if !self.pending.is_empty() {
                return Ok(self.drain_pending(buf));
            }
            // SAFETY: `event` is a live event handle owned by `self`.
            let waited = unsafe { WaitForSingleObject(self.event, WAIT_MS) };
            if waited == WAIT_OBJECT_0 {
                idle_waits = 0;
            } else if waited == WAIT_TIMEOUT {
                idle_waits += 1;
                if idle_waits >= MAX_IDLE_WAITS {
                    return Err(Error::Audio(
                        "the microphone stopped delivering audio".into(),
                    ));
                }
            } else {
                return Err(audio_error("waiting for the microphone")(
                    windows::core::Error::from_thread(),
                ));
            }
        }
    }
}

impl Drop for WasapiStream {
    fn drop(&mut self) {
        // SAFETY: the client and event are live and released exactly once here.
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event);
        }
    }
}

/// Makes COM available to every thread of the process that has not set it up
/// for itself, for as long as the process runs.
///
/// Streams are opened on one thread and used on others, and are kept between
/// sessions when no thread of ours may be running at all. Tying COM to the
/// lifetime of a thread, as a session used to, would end it under them.
pub(crate) fn apartment() -> Result<()> {
    static KEPT: OnceLock<()> = OnceLock::new();
    if KEPT.get().is_none() {
        // SAFETY: the call only counts a use of the multithreaded apartment.
        // The count is never given back, which is the point; counting twice
        // when two threads get here together does no harm.
        unsafe { CoIncrementMTAUsage() }.map_err(audio_error("initializing COM"))?;
        let _ = KEPT.set(());
    }
    Ok(())
}

/// Tells an armed microphone when its stream may have become the wrong one:
/// a device came, went or became the default, or the PC woke from sleep.
///
/// Windows calls in on threads of its own when that happens. Nothing here
/// polls, so watching costs nothing while nothing changes.
pub struct WasapiWatch {
    enumerator: IMMDeviceEnumerator,
    client: IMMNotificationClient,
    power: HPOWERNOTIFY,
}

impl std::fmt::Debug for WasapiWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasapiWatch").finish_non_exhaustive()
    }
}

/// What Windows is given to report sleep and waking with. It is allocated
/// once and never moved, because Windows holds pointers into it.
struct PowerHook {
    subscription: DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS,
    changed: Changed,
}

impl WasapiWatch {
    /// Has Windows report both kinds of change, or neither: a watch that
    /// hears of only one would let a stream be kept that the other kind of
    /// change makes wrong.
    fn new(changed: Changed, flow: EDataFlow) -> Result<Self> {
        apartment()?;
        let client: IMMNotificationClient = DeviceEvents {
            changed: changed.clone(),
            flow,
        }
        .into();
        // SAFETY: COM is available on this thread (`apartment`). The
        // enumerator holds the client until it is taken back, below or in
        // `drop`.
        let enumerator = unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(audio_error("creating the device enumerator"))?;
            enumerator
                .RegisterEndpointNotificationCallback(&client)
                .map_err(audio_error("watching the audio devices"))?;
            enumerator
        };
        let hook = Box::into_raw(Box::new(PowerHook {
            subscription: DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
                Callback: Some(on_power),
                Context: std::ptr::null_mut(),
            },
            changed,
        }));
        // SAFETY: `hook` is a live allocation that nothing else refers to
        // yet, and it is not moved again, so the pointers into it that are
        // made here stay good. With `DEVICE_NOTIFY_CALLBACK` the first
        // argument is a pointer to the subscription.
        let power = unsafe {
            (*hook).subscription.Context = (&raw mut (*hook).changed).cast();
            RegisterSuspendResumeNotification(
                HANDLE((&raw mut (*hook).subscription).cast()),
                DEVICE_NOTIFY_CALLBACK,
            )
        };
        match power {
            // The hook is never freed; see `drop`.
            Ok(power) => Ok(Self {
                enumerator,
                client,
                power,
            }),
            Err(err) => {
                // SAFETY: Windows did not take the hook, so it is ours to
                // free, and the client is taken back from the enumerator
                // that was given it above.
                unsafe {
                    drop(Box::from_raw(hook));
                    let _ = enumerator.UnregisterEndpointNotificationCallback(&client);
                }
                Err(audio_error("watching for the PC to wake up")(err))
            }
        }
    }
}

impl Drop for WasapiWatch {
    fn drop(&mut self) {
        // SAFETY: both registrations were made in `new` and are ended once.
        //
        // The `PowerHook` is deliberately not freed. Windows does not say
        // that ending the registration waits for a call that is under way,
        // so such a call could still be reading the hook afterwards. A watch
        // lives as long as the process in practice, so what is left behind
        // is one small allocation, and a late call finds a `Changed` that is
        // still valid and merely has nobody listening.
        unsafe {
            let _ = UnregisterSuspendResumeNotification(self.power);
            let _ = self
                .enumerator
                .UnregisterEndpointNotificationCallback(&self.client);
        }
    }
}

/// Called by Windows on a thread of its own when the PC sleeps or wakes.
unsafe extern "system" fn on_power(context: *const c_void, kind: u32, _: *const c_void) -> u32 {
    if kind == PBT_APMRESUMEAUTOMATIC && !context.is_null() {
        // SAFETY: the context is the `Changed` inside a `PowerHook`, which
        // is never freed once Windows has it.
        unsafe { &*context.cast::<Changed>() }.resumed();
    }
    0
}

/// What Windows calls when audio devices change. It calls on threads of its
/// own and must not be kept waiting, so each of these only leaves a note.
#[implement(IMMNotificationClient)]
struct DeviceEvents {
    changed: Changed,
    /// Capture devices, except in tests.
    flow: EDataFlow,
}

impl IMMNotificationClient_Impl for DeviceEvents_Impl {
    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> windows::core::Result<()> {
        self.changed.devices();
        Ok(())
    }

    fn OnDeviceAdded(&self, _: &PCWSTR) -> windows::core::Result<()> {
        self.changed.devices();
        Ok(())
    }

    fn OnDeviceRemoved(&self, _: &PCWSTR) -> windows::core::Result<()> {
        self.changed.devices();
        Ok(())
    }

    fn OnDefaultDeviceChanged(
        &self,
        flow: EDataFlow,
        role: ERole,
        _: &PCWSTR,
    ) -> windows::core::Result<()> {
        // One change of the default is reported once for each of three roles.
        if flow == self.flow && role == eConsole {
            self.changed.devices();
        }
        Ok(())
    }

    fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> windows::core::Result<()> {
        Ok(())
    }
}

/// The connected microphone Windows knows by `id`, or `None` for the
/// default one.
///
/// # Safety
///
/// COM must be available on the calling thread.
unsafe fn chosen_device(
    enumerator: &IMMDeviceEnumerator,
    id: &str,
    announce: bool,
) -> Option<IMMDevice> {
    if id.is_empty() {
        return None;
    }
    // SAFETY: the caller has initialized COM; the identifier outlives the call.
    let device = unsafe { enumerator.GetDevice(&HSTRING::from(id)) }.ok();
    // SAFETY: as above, on a device the enumerator returned.
    let active = device
        .as_ref()
        .is_some_and(|device| unsafe { device.GetState() }.is_ok_and(|s| s == DEVICE_STATE_ACTIVE));
    if !active && announce {
        log::write("the chosen microphone is not connected; using the default one");
    }
    device.filter(|_| active)
}

/// Lists the microphones that are connected now, by the name Windows shows
/// for them, in the order Windows gives.
///
/// # Errors
///
/// Returns [`Error::Audio`] if Windows cannot list its audio devices.
pub fn microphones() -> Result<Vec<Microphone>> {
    let _com = ComGuard::new()?;
    // SAFETY: COM is initialized on this thread until `_com` drops, after
    // every object made here has been released.
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(audio_error("creating the device enumerator"))?;
        let devices = enumerator
            .EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)
            .map_err(audio_error("listing the microphones"))?;
        let count = devices
            .GetCount()
            .map_err(audio_error("counting the microphones"))?;
        let mut found = Vec::new();
        for index in 0..count {
            let Ok(device) = devices.Item(index) else {
                continue;
            };
            if let Some(microphone) = describe(&device) {
                found.push(microphone);
            }
        }
        Ok(found)
    }
}

/// The identifier and the name of a device, if Windows gives both.
///
/// # Safety
///
/// COM must be initialized on the calling thread.
unsafe fn describe(device: &IMMDevice) -> Option<Microphone> {
    // SAFETY: as the caller promises.
    unsafe {
        Some(Microphone {
            id: device_id(device)?,
            name: property(device, &PKEY_Device_FriendlyName).filter(|name| !name.is_empty())?,
        })
    }
}

/// What Windows knows a device by.
///
/// # Safety
///
/// COM must be available on the calling thread.
pub(crate) unsafe fn device_id(device: &IMMDevice) -> Option<String> {
    // SAFETY: the caller has initialized COM. The identifier is freed with
    // the allocator that made it, once copied.
    unsafe {
        let raw: PWSTR = device.GetId().ok()?;
        let id = raw.to_string();
        CoTaskMemFree(Some(raw.0.cast_const().cast()));
        id.ok()
    }
}

/// A text property of a device.
///
/// # Safety
///
/// COM must be available on the calling thread.
unsafe fn property(device: &IMMDevice, key: &PROPERTYKEY) -> Option<String> {
    // SAFETY: the caller has initialized COM. The property value is cleared
    // once its text has been taken.
    unsafe {
        let store = device.OpenPropertyStore(STGM_READ).ok()?;
        let mut value = store.GetValue(std::ptr::from_ref(key)).ok()?;
        let text = PropVariantToBSTR(&raw const value).map(|text| text.to_string());
        let _ = PropVariantClear(&raw mut value);
        text.ok()
    }
}

/// Whether a device is reached over Bluetooth. Windows names the part of
/// itself that found a device, and for Bluetooth audio every such name starts
/// with `BTH` (`BTHHFENUM` for the telephone mode a headset microphone uses).
///
/// # Safety
///
/// COM must be available on the calling thread.
unsafe fn is_bluetooth(device: &IMMDevice) -> bool {
    // SAFETY: as the caller promises.
    unsafe { property(device, &PKEY_Device_EnumeratorName) }
        .is_some_and(|name| name.to_ascii_uppercase().starts_with("BTH"))
}

/// Keeps COM initialized on the current thread.
///
/// A thread that already has COM, in another threading model, keeps the one
/// it has: the window thread, where the microphones are listed, is such a
/// thread.
struct ComGuard {
    /// Whether this guard initialized COM and so has to uninitialize it.
    owned: bool,
}

impl ComGuard {
    fn new() -> Result<Self> {
        // SAFETY: balanced by `CoUninitialize` in `Drop`, on the same thread because
        // neither the guard nor its owners are `Send`.
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if result == RPC_E_CHANGED_MODE {
            return Ok(Self { owned: false });
        }
        result.ok().map_err(audio_error("initializing COM"))?;
        Ok(Self { owned: true })
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: pairs with the successful `CoInitializeEx` in `new`.
            unsafe { CoUninitialize() };
        }
    }
}

fn audio_error(action: &'static str) -> impl Fn(windows::core::Error) -> Error {
    move |err| Error::Audio(format!("{action} failed: {}", err.message()))
}

#[cfg(test)]
mod tests {
    //! These run real streams on the audio devices of the PC they run on, so
    //! every one of them is ignored unless asked for by name; what runs with
    //! `cargo test` uses the fake device in `armed.rs`. Each says what it
    //! touches. None of them ever starts a microphone: those that open one
    //! leave it stopped, and those that start a stream do so on what the
    //! speakers play, while playing silence there, and only count what
    //! arrives.
    //!
    //! `cargo test -p vinput-core -- --ignored audio::wasapi`

    use std::process::Command;
    use std::time::{Duration, Instant};

    use windows::Win32::Media::Audio::IAudioRenderClient;

    use super::*;
    use crate::audio::AudioSource;

    impl Wasapi {
        fn loopback() -> Self {
            Self {
                endpoint: Endpoint::Loopback,
            }
        }
    }

    /// Plays silence on the default speakers for as long as it lives, so
    /// that a loopback stream has something to deliver.
    struct Silence {
        client: IAudioClient,
        _render: IAudioRenderClient,
    }

    impl Silence {
        fn play() -> Result<Self> {
            apartment()?;
            let format = WAVEFORMATEX {
                wFormatTag: FORMAT_TAG_PCM,
                nChannels: 1,
                nSamplesPerSec: SAMPLE_RATE_HZ,
                nAvgBytesPerSec: SAMPLE_RATE_HZ * u32::from(BLOCK_ALIGN),
                nBlockAlign: BLOCK_ALIGN,
                wBitsPerSample: 16,
                cbSize: 0,
            };
            // SAFETY: COM is available; the buffer taken is handed back
            // marked silent without being written to.
            unsafe {
                let enumerator: IMMDeviceEnumerator =
                    CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                        .map_err(audio_error("creating the device enumerator"))?;
                let client: IAudioClient = enumerator
                    .GetDefaultAudioEndpoint(eRender, eConsole)
                    .and_then(|device| device.Activate(CLSCTX_ALL, None))
                    .map_err(audio_error("opening the speakers"))?;
                client
                    .Initialize(
                        AUDCLNT_SHAREMODE_SHARED,
                        AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                        // Ten seconds, which outlasts any test here.
                        100_000_000,
                        0,
                        &raw const format,
                        None,
                    )
                    .map_err(audio_error("configuring the speakers"))?;
                let render: IAudioRenderClient = client
                    .GetService()
                    .map_err(audio_error("setting up the speakers"))?;
                let frames = client
                    .GetBufferSize()
                    .map_err(audio_error("sizing the speakers"))?;
                render
                    .GetBuffer(frames)
                    .and_then(|_| render.ReleaseBuffer(frames, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32))
                    .and_then(|()| client.Start())
                    .map_err(audio_error("playing silence"))?;
                Ok(Self {
                    client,
                    _render: render,
                })
            }
        }
    }

    impl Drop for Silence {
        fn drop(&mut self) {
            // SAFETY: the client is live.
            unsafe {
                let _ = self.client.Stop();
            }
        }
    }

    /// Counts the samples `source` delivers in `time`.
    fn count(source: &mut impl AudioSource, time: Duration) -> Result<usize> {
        let start = Instant::now();
        let mut buf = [0i16; 1600];
        let mut total = 0;
        while start.elapsed() < time {
            total += source.read(&mut buf)?;
        }
        Ok(total)
    }

    /// Whether Windows shows this very program as using the microphone. It
    /// keeps that in the registry, per program: an end time of zero under a
    /// start time means "in use now", which is what the taskbar shows.
    fn microphone_in_use() -> bool {
        let Ok(program) = std::env::current_exe() else {
            return false;
        };
        let key = format!(
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone\NonPackaged\{}",
            program.to_string_lossy().replace('\\', "#")
        );
        let Ok(found) = Command::new("reg")
            .args(["query", &key, "/v", "LastUsedTimeStop"])
            .output()
        else {
            return false;
        };
        String::from_utf8_lossy(&found.stdout)
            .split_whitespace()
            .last()
            .is_some_and(|value| value == "0x0")
    }

    /// Opens the default microphone, leaves it stopped for most of a second
    /// and closes it. This is the check that being armed is private.
    #[test]
    #[ignore = "opens the default microphone of this PC, without starting it"]
    fn an_armed_microphone_captures_nothing_and_is_not_shown_as_in_use() {
        let Ok(mut stream) = Wasapi::microphone().open("") else {
            eprintln!("skipped: no microphone that can be opened");
            return;
        };
        assert!(stream.alive());
        assert!(!stream.device().is_empty());
        std::thread::sleep(Duration::from_millis(700));
        // SAFETY: the event is live.
        let signalled = unsafe { WaitForSingleObject(stream.event, 0) } == WAIT_OBJECT_0;
        assert!(!signalled, "a stopped stream is never told of audio");
        assert_eq!(stream.fetch_packet().ok(), Some(false), "and has none");
        assert!(!microphone_in_use());
        drop(stream);
        assert!(!microphone_in_use());
    }

    /// Opens the default microphone, leaves it stopped and closes it.
    #[test]
    #[ignore = "opens the default microphone of this PC, without starting it"]
    fn the_microphone_a_stream_would_be_kept_on_is_the_one_that_is_opened() {
        let backend = Wasapi::microphone();
        let Ok(stream) = backend.open("") else {
            eprintln!("skipped: no microphone that can be opened");
            return;
        };
        if stream.may_stay_armed() {
            assert_eq!(backend.standby("").as_deref(), Some(stream.device()));
            // A microphone that is not there means the default one.
            assert_eq!(
                backend.standby("{no-such-device}").as_deref(),
                Some(stream.device())
            );
        } else {
            assert_eq!(backend.standby(""), None);
        }
    }

    /// Plays silence on the default speakers for a second and listens to
    /// what they play. No microphone is opened.
    #[test]
    #[ignore = "plays silence on the default speakers of this PC and listens to them"]
    fn a_real_stream_starts_stops_and_starts_again_with_nothing_left_over() -> Result<()> {
        let Ok(_silence) = Silence::play() else {
            eprintln!("skipped: no speakers to listen to");
            return Ok(());
        };
        let armed = Armed::with(Wasapi::loopback(), "");

        let stop = StopHandle::new();
        let mut source = armed.open(stop.clone())?;
        let heard = count(&mut source, Duration::from_millis(300))?;
        // 4800 samples in real time; the first packet takes a moment.
        assert!((3200..=6400).contains(&heard), "{heard} samples in 300 ms");

        // The reader is away for 300 ms and the session is stopped. What
        // was captured meanwhile still comes, and then the source ends.
        std::thread::sleep(Duration::from_millis(300));
        stop.stop();
        let mut buf = [0i16; 1600];
        let mut late = 0;
        loop {
            let read = source.read(&mut buf)?;
            if read == 0 {
                break;
            }
            late += read;
        }
        assert!(
            (3200..=6400).contains(&late),
            "{late} samples after the stop"
        );

        // Leave 300 ms unread again, and drop the session with it.
        drop(source);
        armed.settle();
        let source = armed.open(StopHandle::new())?;
        std::thread::sleep(Duration::from_millis(300));
        drop(source);
        armed.settle();

        // The next session gets the same stream and none of that audio:
        // 100 ms brings 1600 samples, not the 6400 it would with the rest.
        let mut source = armed.open(StopHandle::new())?;
        let heard = count(&mut source, Duration::from_millis(100))?;
        assert!((320..=3200).contains(&heard), "{heard} samples in 100 ms");

        // A reader that fell behind catches up in one read.
        std::thread::sleep(Duration::from_millis(100));
        let mut buf = [0i16; 1600];
        let caught_up = source.read(&mut buf)?;
        assert!(caught_up >= 800, "{caught_up} samples after 100 ms away");
        Ok(())
    }

    /// Keeps a stream on the real microphone, never started, for a
    /// minute, so that what that costs can be read from outside: the
    /// context switches of this process while it runs should not grow.
    #[test]
    #[ignore = "a measurement; keeps the default microphone of this PC open, without starting it"]
    fn an_armed_microphone_at_rest() {
        let Ok(stream) = Wasapi::microphone().open("") else {
            eprintln!("skipped: no microphone that can be opened");
            return;
        };
        eprintln!("armed in process {}", std::process::id());
        std::thread::sleep(Duration::from_secs(60));
        assert!(!microphone_in_use());
        drop(stream);
    }

    fn median(mut times: Vec<Duration>) -> Duration {
        times.sort();
        times[times.len() / 2]
    }

    /// Prints what keeping the microphone ready saves. It opens the default
    /// microphone fifteen times without starting it, then plays silence on
    /// the default speakers and listens to them. Run by hand:
    /// `cargo test -p vinput-core --release -- --ignored --nocapture what_arming_saves`.
    #[test]
    #[ignore = "a measurement; opens the default microphone of this PC, without starting it"]
    fn what_arming_saves() -> Result<()> {
        const RUNS: usize = 15;
        // Everything a session used to do before `Start`, on a thread of its
        // own as it was, on the real microphone, which is never started.
        let mut opening = Vec::new();
        for _ in 0..RUNS {
            let begun = Instant::now();
            let opened = std::thread::spawn(|| Wasapi::microphone().open("").map(drop)).join();
            if !matches!(opened, Ok(Ok(()))) {
                eprintln!("skipped: no microphone that can be opened");
                return Ok(());
            }
            opening.push(begun.elapsed());
            std::thread::sleep(Duration::from_millis(100));
        }
        eprintln!(
            "microphone, everything but Start (what arming saves): median {:?}",
            median(opening)
        );

        let Ok(_silence) = Silence::play() else {
            return Ok(());
        };
        let armed = Armed::with(Wasapi::loopback(), "");
        let (mut kept, mut afresh, mut first) = (Vec::new(), Vec::new(), Vec::new());
        let mut buf = [0i16; 1600];
        for _ in 0..RUNS {
            armed.settle();
            let begun = Instant::now();
            let mut source = armed.open(StopHandle::new())?;
            kept.push(begun.elapsed());
            source.read(&mut buf)?;
            first.push(begun.elapsed());
            drop(source);

            let begun = Instant::now();
            let mut stream = Wasapi::loopback().open("")?;
            stream.start()?;
            afresh.push(begun.elapsed());
            drop(stream);
        }
        eprintln!(
            "loopback, key press to capturing: kept ready {:?}, opened afresh {:?}; first audio after {:?}",
            median(kept),
            median(afresh),
            median(first)
        );
        Ok(())
    }
}
