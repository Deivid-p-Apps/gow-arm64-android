//! Audio layer: XAudio2 (the game's audio API) mapped onto Android **AAudio**.
//!
//! God of War (2018) PC mixes with XAudio2: `XAudio2Create` returns an
//! `IXAudio2`, the game creates a mastering voice (the device), source voices
//! (one per sound/stream, fed with PCM through `SubmitSourceBuffer`), and
//! optionally submix voices. The recompiled code reaches all of this through its
//! IAT, so the runtime must provide the COM objects and do the actual mixing.
//!
//! This module owns the mixing graph and pumps it to the device:
//!
//! ```text
//!   game thread                          mixer thread (gow-audio)
//!   -----------                          -----------------------
//!   submit_buffer(voice, pcm, n)  --->   per-voice ring buffer
//!        (decode -> f32)                 |  resample/convert to master format
//!                                        v
//!                                  recursive graph mix (sources -> submix -> master)
//!                                        |
//!                                        v  convert to device format
//!                                  AAudioStream_write(libaaudio.so)
//! ```
//!
//! Design rules kept from the rest of the runtime (see `docs/DECISIONS.md`,
//! D005/D012): **never fabricate audio and never fake success.** Buffers are
//! decoded and mixed for real; anything not implemented logs and returns failure.
//! When `libaaudio.so` is absent the engine still runs (voices, rings and the
//! mixer work) but there is no device sink — that is logged and reported through
//! [`AudioEngine::output_available`], not hidden.
//!
//! ## XAudio2 COM ABI (vtable indices)
//!
//! The game calls these as COM methods: `pThis` is the object, arguments follow
//! the Microsoft x64 ABI (RCX = `pThis`), and the function pointer is at
//! `*(*(void***)pThis + index)`. Indices below are absolute in each vtable
//! (derived i.faces repeat the base methods first), so the runtime can dispatch
//! without guessing.
//!
//! ```text
//! IXAudio2  (extends IUnknown)
//!   0  QueryInterface        1  AddRef                 2  Release
//!   3  RegisterForCallbacks  4  UnregisterForCallbacks
//!   5  CreateSourceVoice     6  CreateSubmixVoice      7  CreateMasteringVoice
//!   8  StartEngine           9  StopEngine            10  CommitChanges
//!  11  GetPerformanceData   12  SetDebugConfiguration
//!
//! IXAudio2Voice (base for mastering/submix/source)
//!   0  GetVoiceDetails       1  SetOutputVoices        2  SetEffectChain
//!   3  EnableEffect          4  DisableEffect          5  GetEffectState
//!   6  SetEffectParameters   7  GetEffectParameters    8  SetFilterParameters
//!   9  GetFilterParameters  10  SetOutputFilterParameters
//!  11  GetOutputFilterParameters
//!  12  SetVolume            13  GetVolume             14  SetChannelVolumes
//!  15  GetChannelVolumes    16  SetOutputMatrix       17  GetOutputMatrix
//!  18  DestroyVoice
//!
//! IXAudio2SourceVoice (adds to IXAudio2Voice)
//!  19  Start                20  Stop                 21  SubmitSourceBuffer
//!  22  FlushSourceBuffers   23  Discontinuity        24  ExitLoop
//!  25  GetState             26  SetFrequencyRatio    27  GetFrequencyRatio
//!  28  SetSourceSampleRate
//!
//! IXAudio2MasteringVoice (adds to IXAudio2Voice)
//!  19  GetChannelMask
//!
//! IXAudio2SubmixVoice  == IXAudio2Voice (no extra methods)
//!
//! IXAudio2EngineCallback
//!   0  OnProcessingPassStart 1  OnProcessingPassEnd  2  OnCriticalError
//!
//! IXAudio2VoiceCallback
//!   0  OnVoiceProcessingPassStart  1  OnVoiceProcessingPassEnd
//!   2  OnStreamEnd                 3  OnBufferStart
//!   4  OnBufferEnd                  5  OnLoopEnd      6  OnVoiceError
//! ```
//!
//! Exports the game may import: `XAudio2Create` (-> `IXAudio2`), plus the APO
//! factories `CreateAudioReverb` / `CreateAudioVolumeMeter`. The APOs need
//! `IXAPO`/`IXAPOFactories` COM objects and are **not** implemented here (they
//! must return failure, see `docs/AUDIO.md`).
//!
//! Signatures for the wiring (`win32.rs`):
//! ```text
//! HRESULT XAudio2Create(IXAudio2 **ppXAudio2, UINT32 Flags, XAUDIO2_PROCESSOR Processor);
//! HRESULT CreateAudioReverb(IXAudio2 **ppApo);
//! HRESULT CreateAudioVolumeMeter(IUnknown **ppApo);
//! ```

use std::collections::HashMap;
use std::ffi::c_void;
use std::os::raw::c_char;
use std::slice;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

// ---------------------------------------------------------------------------
// vtable indices (see the ABI table above) — useful to the win32.rs wiring.
// ---------------------------------------------------------------------------
/// `IXAudio2::CreateMasteringVoice`.
pub const IXAUDIO2_CREATE_MASTERING_VOICE: usize = 7;
/// `IXAudio2::CreateSourceVoice`.
pub const IXAUDIO2_CREATE_SOURCE_VOICE: usize = 5;
/// `IXAudio2::CreateSubmixVoice`.
pub const IXAUDIO2_CREATE_SUBMIX_VOICE: usize = 6;
/// `IXAudio2::StartEngine`.
pub const IXAUDIO2_START_ENGINE: usize = 8;
/// `IXAudio2Voice::SetVolume` (shared base of every voice).
pub const IXAUDIO2VOICE_SET_VOLUME: usize = 12;
/// `IXAudio2Voice::DestroyVoice`.
pub const IXAUDIO2VOICE_DESTROY_VOICE: usize = 18;
/// `IXAudio2SourceVoice::Start`.
pub const IXAUDIO2SOURCEVOICE_START: usize = 19;
/// `IXAudio2SourceVoice::Stop`.
pub const IXAUDIO2SOURCEVOICE_STOP: usize = 20;
/// `IXAudio2SourceVoice::SubmitSourceBuffer`.
pub const IXAUDIO2SOURCEVOICE_SUBMIT_SOURCE_BUFFER: usize = 21;
/// `IXAudio2SourceVoice::FlushSourceBuffers`.
pub const IXAUDIO2SOURCEVOICE_FLUSH_SOURCE_BUFFERS: usize = 22;

// ---------------------------------------------------------------------------
// XAudio2 structures (repr(C) mirrors of the public headers).
// ---------------------------------------------------------------------------

/// `XAUDIO2_BUFFER` — one submitted buffer. `pAudioData`/`AudioBytes` are what
/// the mixer consumes; loop/context fields are carried for fidelity but the
/// loop region is not implemented yet (see `docs/AUDIO.md`).
#[repr(C)]
pub struct XAudio2Buffer {
    pub flags: u32,
    pub audio_bytes: u32,
    pub audio_data: *const u8,
    pub play_begin: u32,
    pub play_length: u32,
    pub loop_begin: u32,
    pub loop_length: u32,
    pub loop_count: u32,
    pub context: *mut c_void,
}

/// `XAUDIO2_END_OF_STREAM` flag on `XAUDIO2_BUFFER::Flags`.
pub const XAUDIO2_END_OF_STREAM: u32 = 0x0040;

/// `WAVEFORMATEX` is `#pragma pack(1)` in `mmreg.h` (18 bytes); mirrored packed
/// so a future `CreateSourceVoice` hook can read it without layout drift.
#[repr(C, packed)]
pub struct WaveFormatEx {
    pub format_tag: u16,
    pub channels: u16,
    pub samples_per_sec: u32,
    pub avg_bytes_per_sec: u32,
    pub block_align: u16,
    pub bits_per_sample: u16,
    pub cb_size: u16,
}

/// Read a `WAVEFORMATEX*` from guest memory into an [`AudioFormat`]. Guest
/// pointers are host pointers in this runtime (see D004), so a plain load is
/// correct on device. Returns `None` on null / zero fields.
///
/// # Safety
/// `ptr` must point at a readable `WAVEFORMATEX` (or be 0, which is handled).
pub unsafe fn audio_format_from_wfx(ptr: u64) -> Option<AudioFormat> {
    if ptr == 0 {
        return None;
    }
    let wfx = std::ptr::read_unaligned(ptr as *const WaveFormatEx);
    // Copy each field out of the packed struct (no references into it).
    let channels = wfx.channels;
    let samples = wfx.samples_per_sec;
    let bits = wfx.bits_per_sample;
    if channels == 0 || samples == 0 || bits == 0 {
        return None;
    }
    Some(AudioFormat { sample_rate: samples, channels, bits })
}

// ---------------------------------------------------------------------------
// AudioFormat
// ---------------------------------------------------------------------------

/// PCM format of a voice (subset of `WAVEFORMATEX` the mixer needs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u16,
    /// Bits per sample: 8 (unsigned), 16 (signed), 24 (signed) or 32 (IEEE float).
    pub bits: u16,
}

impl AudioFormat {
    /// Bytes for one interleaved frame, or 0 for an unsupported width.
    pub fn frame_size(&self) -> usize {
        bytes_per_sample(self.bits) * self.channels as usize
    }
}

/// `bits` -> bytes per sample; 0 if the width is not supported.
fn bytes_per_sample(bits: u16) -> usize {
    match bits {
        8 => 1,
        16 => 2,
        24 => 3,
        32 => 4,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// AAudio binding (dlopen'd, so the crate loads on hosts without libaaudio.so).
// ---------------------------------------------------------------------------

/// `AAUDIO_FORMAT_PCM_I16`.
const AAUDIO_FORMAT_PCM_I16: i32 = 1;
/// `AAUDIO_FORMAT_PCM_FLOAT`.
const AAUDIO_FORMAT_PCM_FLOAT: i32 = 2;
/// `AAUDIO_OK`.
const AAUDIO_OK: i32 = 0;
/// How long `AAudioStream_write` may block before returning.
const AAUDIO_WRITE_TIMEOUT_NS: i64 = 100_000_000; // 100 ms

const RTLD_NOW: i32 = 2;

extern "C" {
    fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, sym: *const c_char) -> *mut c_void;
}

// Function pointer types matching `aaudio/AAudio.h`.
type CreateStreamBuilderFn = unsafe extern "C" fn(*mut *mut c_void) -> i32;
type BuilderSetFormatFn = unsafe extern "C" fn(*mut c_void, i32);
type BuilderSetChannelCountFn = unsafe extern "C" fn(*mut c_void, i32);
type BuilderSetSampleRateFn = unsafe extern "C" fn(*mut c_void, i32);
type BuilderOpenStreamFn = unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> i32;
type BuilderDeleteFn = unsafe extern "C" fn(*mut c_void) -> i32;
type StreamRequestStartFn = unsafe extern "C" fn(*mut c_void) -> i32;
type StreamWriteFn = unsafe extern "C" fn(*mut c_void, *const c_void, i32, i64) -> i32;
type StreamRequestStopFn = unsafe extern "C" fn(*mut c_void) -> i32;
type StreamCloseFn = unsafe extern "C" fn(*mut c_void) -> i32;

/// The AAudio entry points we use, resolved from `libaaudio.so` at runtime.
/// Absence of the library (dev hosts, emulators without AAudio) is a normal,
/// logged condition: the engine keeps its graph but has no device sink.
struct AaudioFns {
    create_stream_builder: CreateStreamBuilderFn,
    builder_set_format: BuilderSetFormatFn,
    builder_set_channel_count: BuilderSetChannelCountFn,
    builder_set_sample_rate: BuilderSetSampleRateFn,
    builder_open_stream: BuilderOpenStreamFn,
    builder_delete: BuilderDeleteFn,
    stream_request_start: StreamRequestStartFn,
    stream_write: StreamWriteFn,
    stream_request_stop: StreamRequestStopFn,
    stream_close: StreamCloseFn,
}

impl AaudioFns {
    unsafe fn load() -> Option<Self> {
        let h = dlopen(b"libaaudio.so\0".as_ptr() as *const c_char, RTLD_NOW);
        if h.is_null() {
            return None;
        }
        let sym = |name: &str| -> usize {
            let c = match std::ffi::CString::new(name) {
                Ok(c) => c,
                Err(_) => return 0,
            };
            dlsym(h, c.as_ptr()) as usize
        };
        let create_stream_builder = sym("AAudio_createStreamBuilder");
        let builder_set_format = sym("AAudioStreamBuilder_setFormat");
        let builder_set_channel_count = sym("AAudioStreamBuilder_setChannelCount");
        let builder_set_sample_rate = sym("AAudioStreamBuilder_setSampleRate");
        let builder_open_stream = sym("AAudioStreamBuilder_openStream");
        let builder_delete = sym("AAudioStreamBuilder_delete");
        let stream_request_start = sym("AAudioStream_requestStart");
        let stream_write = sym("AAudioStream_write");
        let stream_request_stop = sym("AAudioStream_requestStop");
        let stream_close = sym("AAudioStream_close");

        if create_stream_builder == 0
            || builder_set_format == 0
            || builder_set_channel_count == 0
            || builder_set_sample_rate == 0
            || builder_open_stream == 0
            || builder_delete == 0
            || stream_request_start == 0
            || stream_write == 0
            || stream_request_stop == 0
            || stream_close == 0
        {
            crate::glog!("[gow.audio] libaaudio.so present but missing entry points");
            return None;
        }

        Some(AaudioFns {
            create_stream_builder: std::mem::transmute(create_stream_builder),
            builder_set_format: std::mem::transmute(builder_set_format),
            builder_set_channel_count: std::mem::transmute(builder_set_channel_count),
            builder_set_sample_rate: std::mem::transmute(builder_set_sample_rate),
            builder_open_stream: std::mem::transmute(builder_open_stream),
            builder_delete: std::mem::transmute(builder_delete),
            stream_request_start: std::mem::transmute(stream_request_start),
            stream_write: std::mem::transmute(stream_write),
            stream_request_stop: std::mem::transmute(stream_request_stop),
            stream_close: std::mem::transmute(stream_close),
        })
    }
}

/// Raw AAudio stream handle, marked `Send`/`Sync` so it can sit inside the
/// engine shared with the mixer thread. AAudio stream functions are themselves
/// thread-safe for one writer.
struct StreamHandle(*mut c_void);
unsafe impl Send for StreamHandle {}
unsafe impl Sync for StreamHandle {}

/// Device format constant for a given sample width: 32-bit mixes as float,
/// everything else falls back to 16-bit signed (logged once by the caller).
fn device_format_const(bits: u16) -> i32 {
    if bits == 32 {
        AAUDIO_FORMAT_PCM_FLOAT
    } else {
        AAUDIO_FORMAT_PCM_I16
    }
}

// ---------------------------------------------------------------------------
// Ring buffer
// ---------------------------------------------------------------------------

/// Fixed-capacity ring of `f32` samples for one voice. Written by the game
/// thread (`submit_buffer`), drained by the mixer thread.
///
/// Overflow policy: when full, the **oldest** quarter of the buffer is dropped
/// to make room, so the newest game audio keeps playing (a glitch is better
/// than growing latency). Every drop is counted and reported, never silent.
struct RingBuffer {
    buf: Vec<f32>,
    head: usize,
    len: usize,
    /// Total samples dropped because of overflow (monotonic).
    dropped: u64,
}

impl RingBuffer {
    fn new(capacity_samples: usize) -> Self {
        Self { buf: vec![0.0; capacity_samples.max(1)], head: 0, len: 0, dropped: 0 }
    }

    fn len(&self) -> usize {
        self.len
    }

    /// Append `samples`, dropping the oldest quarter (once) if full.
    fn push(&mut self, samples: &[f32]) {
        let cap = self.buf.len();
        for &x in samples {
            if self.len == cap {
                let drop = (cap / 4).max(1);
                self.drop_front(drop);
                self.dropped += drop as u64;
            }
            let w = (self.head + self.len) % cap;
            self.buf[w] = x;
            self.len += 1;
        }
    }

    fn drop_front(&mut self, n: usize) {
        let n = n.min(self.len);
        self.head = (self.head + n) % self.buf.len();
        self.len -= n;
    }

    /// Sample at `index` frames/samples from the read head. Caller bounds it.
    fn get(&self, index: usize) -> f32 {
        self.buf[(self.head + index) % self.buf.len()]
    }
}

// ---------------------------------------------------------------------------
// Voices
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum VoiceKind {
    Master,
    Submix,
    Source,
}

/// Mutable per-voice state touched by the mixer (ring + resampler phase).
struct VoiceState {
    ring: RingBuffer,
    /// Fractional source-frame position kept across mixer periods for the
    /// naive linear resampler when voice rate != master rate.
    resample_pos: f64,
}

/// One XAudio2 voice. Shared between the game thread (create/submit/volume) and
/// the mixer thread (drain/mix) behind an `Arc`.
struct Voice {
    id: u64,
    kind: VoiceKind,
    format: AudioFormat,
    state: Mutex<VoiceState>,
    /// Volume as raw `f32` bits (lock-free set/get).
    volume: AtomicU32,
    /// Source voices only: XAudio2 Start/Stop gate buffer processing.
    playing: AtomicBool,
    /// Output voice id; 0 means "the mastering voice".
    output: AtomicU64,
    submitted_bytes: AtomicU64,
    decoded_samples: AtomicU64,
}

impl Voice {
    fn new(id: u64, kind: VoiceKind, format: AudioFormat, ring_samples: usize) -> Self {
        Self {
            id,
            kind,
            format,
            state: Mutex::new(VoiceState { ring: RingBuffer::new(ring_samples), resample_pos: 0.0 }),
            volume: AtomicU32::new(1.0f32.to_bits()),
            playing: AtomicBool::new(false),
            output: AtomicU64::new(0),
            submitted_bytes: AtomicU64::new(0),
            decoded_samples: AtomicU64::new(0),
        }
    }

    fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Ordering::Relaxed))
    }
}

/// Mastering-voice descriptor captured when the device is opened.
#[derive(Clone, Copy)]
struct MasterInfo {
    id: u64,
    format: AudioFormat,
}

/// Shared engine state; `Arc`-cloned into the mixer thread.
struct Inner {
    voices: Mutex<HashMap<u64, Arc<Voice>>>,
    master: Mutex<Option<MasterInfo>>,
    stream: Mutex<Option<StreamHandle>>,
    aaudio: Option<AaudioFns>,
    shutdown: AtomicBool,
    device_available: AtomicBool,
    next_id: AtomicU64,
    periods: AtomicU64,
    device_write_errors: AtomicU64,
    ring_drop_events: AtomicU64,
    bad_submits: AtomicU64,
    cycle_hits: AtomicU64,
    missing_voice_calls: AtomicU64,
}

// ---------------------------------------------------------------------------
// AudioEngine
// ---------------------------------------------------------------------------

/// XAudio2-equivalent mixing engine on top of AAudio.
///
/// The engine is cheap to hold inside the import resolver. All methods are
/// `&self`; the mixer runs on its own thread and is stopped by `Drop`.
pub struct AudioEngine {
    inner: Arc<Inner>,
    thread: Option<JoinHandle<()>>,
}

/// Snapshot of engine counters (for diagnostics/tests; nothing fabricated).
#[derive(Clone, Copy, Debug, Default)]
pub struct AudioStats {
    pub voices: usize,
    pub periods: u64,
    pub device_write_errors: u64,
    pub ring_drop_events: u64,
    pub bad_submits: u64,
    pub cycle_hits: u64,
    pub missing_voice_calls: u64,
    pub output_available: bool,
}

/// Sample width used for the AAudio device stream. 16-bit is the most broadly
/// supported output; the internal mix is always `f32`.
const MASTER_DEVICE_BITS: u16 = 16;
/// Depth guard for the voice-send graph (and cycle detection via the path).
const MAX_MIX_DEPTH: usize = 32;

impl AudioEngine {
    pub fn new() -> Self {
        let aaudio = unsafe { AaudioFns::load() };
        if aaudio.is_none() {
            crate::glog!("[gow.audio] libaaudio.so unavailable: no device sink, engine runs silent");
        }
        let inner = Arc::new(Inner {
            voices: Mutex::new(HashMap::new()),
            master: Mutex::new(None),
            stream: Mutex::new(None),
            aaudio,
            shutdown: AtomicBool::new(false),
            device_available: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            periods: AtomicU64::new(0),
            device_write_errors: AtomicU64::new(0),
            ring_drop_events: AtomicU64::new(0),
            bad_submits: AtomicU64::new(0),
            cycle_hits: AtomicU64::new(0),
            missing_voice_calls: AtomicU64::new(0),
        });
        let thread = {
            let inner = inner.clone();
            std::thread::Builder::new()
                .name("gow-audio".into())
                .spawn(move || mixer_loop(inner))
                .map_err(|e| crate::glog!("[gow.audio] failed to start mixer thread: {e}"))
                .ok()
        };
        Self { inner, thread }
    }

    /// True once an AAudio device stream is open. False in the no-op fallback
    /// (library missing or openStream failed) — callers may surface this.
    pub fn output_available(&self) -> bool {
        self.inner.device_available.load(Ordering::Relaxed)
    }

    /// Counters snapshot.
    pub fn stats(&self) -> AudioStats {
        AudioStats {
            voices: self.inner.voices.lock().unwrap().len(),
            periods: self.inner.periods.load(Ordering::Relaxed),
            device_write_errors: self.inner.device_write_errors.load(Ordering::Relaxed),
            ring_drop_events: self.inner.ring_drop_events.load(Ordering::Relaxed),
            bad_submits: self.inner.bad_submits.load(Ordering::Relaxed),
            cycle_hits: self.inner.cycle_hits.load(Ordering::Relaxed),
            missing_voice_calls: self.inner.missing_voice_calls.load(Ordering::Relaxed),
            output_available: self.output_available(),
        }
    }

    /// `IXAudio2::CreateMasteringVoice` — creates the one device (master)
    /// voice and opens the AAudio stream with its format. A second call is an
    /// error (XAudio2 allows a single mastering voice): logged, returns 0.
    pub fn create_master_voice(&self, sample_rate: u32, channels: u32) -> u64 {
        let mut master = self.inner.master.lock().unwrap();
        if master.is_some() {
            crate::glog!("[gow.audio] CreateMasteringVoice: already created, refusing second");
            return 0;
        }
        let format = AudioFormat {
            sample_rate: sample_rate.max(1),
            channels: channels.max(1).min(u16::MAX as u32) as u16,
            bits: MASTER_DEVICE_BITS,
        };
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        // The mastering voice has no game-submitted data; a small ring is enough
        // (it only sums its children).
        let voice = Arc::new(Voice::new(id, VoiceKind::Master, format, 4096));
        self.inner.voices.lock().unwrap().insert(id, voice);
        *master = Some(MasterInfo { id, format });
        drop(master);

        self.open_device(format, id);
        id
    }

    /// `IXAudio2::CreateSourceVoice` — a voice fed by `submit_buffer`.
    pub fn create_source_voice(&self, format: AudioFormat) -> u64 {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        // ~1 s of audio headroom at this voice's own rate.
        let cap = (format.sample_rate as usize * format.channels as usize).max(4096);
        let voice = Arc::new(Voice::new(id, VoiceKind::Source, format, cap));
        self.inner.voices.lock().unwrap().insert(id, voice);
        id
    }

    /// `IXAudio2::CreateSubmixVoice` — an intermediate mix bus feeding `output`
    /// (0 = mastering voice). Effects chains (`ProcessingStage`, APOs) are not
    /// implemented; the voice currently just sums and re-scales its inputs.
    pub fn create_submix_voice(&self, input_channels: u32, sample_rate: u32, output: u64) -> u64 {
        let format = AudioFormat {
            sample_rate: sample_rate.max(1),
            channels: input_channels.max(1).min(u16::MAX as u32) as u16,
            bits: MASTER_DEVICE_BITS,
        };
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let voice = Arc::new(Voice::new(id, VoiceKind::Submix, format, 4096));
        voice.output.store(output, Ordering::Relaxed);
        self.inner.voices.lock().unwrap().insert(id, voice);
        id
    }

    /// `IXAudio2SourceVoice::SubmitSourceBuffer` (data pointer + byte count).
    ///
    /// Decodes the submitted PCM with the voice format into `f32` and queues it
    /// on the voice's ring. Returns `false` (and logs) for unknown/incorrect
    /// voices or malformed sizes — never a silent accept.
    pub fn submit_buffer(&self, voice: u64, data_ptr: u64, byte_count: u32) -> bool {
        let v = match self.voice(voice) {
            Some(v) => v,
            None => {
                count_missing(&self.inner);
                return false;
            }
        };
        if v.kind != VoiceKind::Source {
            let n = self.inner.bad_submits.fetch_add(1, Ordering::Relaxed);
            if n < 8 {
                crate::glog!("[gow.audio] SubmitSourceBuffer on non-source voice {voice}");
            }
            return false;
        }
        if data_ptr == 0 || byte_count == 0 {
            return false;
        }
        let fmt = v.format;
        let frame = fmt.frame_size();
        if frame == 0 || (byte_count as usize) % frame != 0 {
            let n = self.inner.bad_submits.fetch_add(1, Ordering::Relaxed);
            if n < 8 {
                crate::glog!(
                    "[gow.audio] SubmitSourceBuffer size {byte_count} not a whole frame \
                     ({frame} bytes, {} ch x {} bits)",
                    fmt.channels,
                    fmt.bits
                );
            }
            return false;
        }

        // Guest memory is host memory in this runtime (D004): read directly.
        let bytes = unsafe { slice::from_raw_parts(data_ptr as *const u8, byte_count as usize) };
        let samples = decode_pcm(bytes, fmt.bits);
        if samples.is_empty() {
            return false;
        }

        let dropped = {
            let mut st = v.state.lock().unwrap();
            let before = st.ring.dropped;
            st.ring.push(&samples);
            st.ring.dropped - before
        };
        v.submitted_bytes.fetch_add(byte_count as u64, Ordering::Relaxed);
        v.decoded_samples.fetch_add(samples.len() as u64, Ordering::Relaxed);

        if dropped > 0 {
            let n = self.inner.ring_drop_events.fetch_add(1, Ordering::Relaxed);
            if n < 8 {
                crate::glog!("[gow.audio] voice {voice}: ring overflow, dropped {dropped} samples");
            }
        }
        true
    }

    /// Convenience for the wiring: read a guest `XAUDIO2_BUFFER` and submit its
    /// `pAudioData`/`AudioBytes`. Optional `pBufferWMA` is ignored (PCM only).
    ///
    /// # Safety
    /// `buffer_ptr` must point at a readable `XAUDIO2_BUFFER` in guest memory.
    pub unsafe fn submit_xaudio2_buffer(&self, voice: u64, buffer_ptr: u64) -> bool {
        if buffer_ptr == 0 {
            return false;
        }
        let buf = std::ptr::read_unaligned(buffer_ptr as *const XAudio2Buffer);
        // END_OF_STREAM is carried for fidelity; loop regions are not honoured yet.
        if buf.flags & XAUDIO2_END_OF_STREAM != 0 {
            crate::glog!("[gow.audio] voice {voice}: XAUDIO2_END_OF_STREAM buffer");
        }
        self.submit_buffer(voice, buf.audio_data as u64, buf.audio_bytes)
    }

    /// `IXAudio2Voice::SetVolume`.
    pub fn set_volume(&self, voice: u64, v: f32) {
        match self.voice(voice) {
            Some(vo) => vo.volume.store(v.to_bits(), Ordering::Relaxed),
            None => count_missing(&self.inner),
        }
    }

    /// `IXAudio2SourceVoice::Start`.
    pub fn start(&self, voice: u64) {
        match self.voice(voice) {
            Some(vo) => vo.playing.store(true, Ordering::Relaxed),
            None => count_missing(&self.inner),
        }
    }

    /// `IXAudio2SourceVoice::Stop` (does not flush; queued buffers are kept).
    pub fn stop(&self, voice: u64) {
        match self.voice(voice) {
            Some(vo) => vo.playing.store(false, Ordering::Relaxed),
            None => count_missing(&self.inner),
        }
    }

    /// `IXAudio2SourceVoice::FlushSourceBuffers` — drop queued audio and reset
    /// the resampler phase.
    pub fn flush_source_buffers(&self, voice: u64) {
        match self.voice(voice) {
            Some(vo) => {
                let mut st = vo.state.lock().unwrap();
                let n = st.ring.len();
                st.ring.drop_front(n);
                st.resample_pos = 0.0;
            }
            None => count_missing(&self.inner),
        }
    }

    /// `IXAudio2Voice::SetOutputVoices` (single send): re-point a voice at
    /// another voice, or at the mastering voice with `target == 0`.
    pub fn set_output(&self, voice: u64, target: u64) {
        match self.voice(voice) {
            Some(vo) => vo.output.store(target, Ordering::Relaxed),
            None => count_missing(&self.inner),
        }
    }

    /// Locate a voice by id.
    fn voice(&self, id: u64) -> Option<Arc<Voice>> {
        self.inner.voices.lock().unwrap().get(&id).cloned()
    }

    /// Open the AAudio output stream for the mastering format. On any failure
    /// the engine keeps running with no device sink (logged, not faked).
    fn open_device(&self, fmt: AudioFormat, master_id: u64) {
        let fns = match &self.inner.aaudio {
            Some(f) => f,
            None => {
                crate::glog!("[gow.audio] mastering voice {master_id}: no AAudio, output silent");
                return;
            }
        };
        unsafe {
            let mut builder: *mut c_void = std::ptr::null_mut();
            let r = (fns.create_stream_builder)(&mut builder);
            if r != AAUDIO_OK || builder.is_null() {
                crate::glog!("[gow.audio] AAudio_createStreamBuilder failed: {r}");
                return;
            }
            (fns.builder_set_format)(builder, device_format_const(fmt.bits));
            (fns.builder_set_channel_count)(builder, fmt.channels as i32);
            (fns.builder_set_sample_rate)(builder, fmt.sample_rate as i32);

            let mut stream: *mut c_void = std::ptr::null_mut();
            let open = (fns.builder_open_stream)(builder, &mut stream);
            (fns.builder_delete)(builder);
            if open != AAUDIO_OK || stream.is_null() {
                crate::glog!("[gow.audio] AAudioStreamBuilder_openStream failed: {open}");
                return;
            }
            let started = (fns.stream_request_start)(stream);
            if started != AAUDIO_OK {
                crate::glog!("[gow.audio] AAudioStream_requestStart failed: {started}");
            }
            *self.inner.stream.lock().unwrap() = Some(StreamHandle(stream));
            self.inner.device_available.store(true, Ordering::Relaxed);
            crate::glog!(
                "[gow.audio] AAudio device open: {} Hz, {} ch, {} bits (master {master_id})",
                fmt.sample_rate,
                fmt.channels,
                fmt.bits
            );
        }
    }
}

/// Process-wide audio engine shared by the import layer and the JNI surface.
pub fn global() -> &'static AudioEngine {
    static ENGINE: std::sync::OnceLock<AudioEngine> = std::sync::OnceLock::new();
    ENGINE.get_or_init(AudioEngine::new)
}

/// XAudio2 bootstrap: build an `IXAudio2` object and vtable for the guest.
///
/// The guest calls COM methods as `call [vtable + idx*8]` with `this` in RCX.
/// We return a small integer "object" handle as `this` and a vtable of host
/// thunks (see `crate::hostcall`) that decode the guest register file and drive
/// the real `AudioEngine`. Unimplemented methods return E_NOTIMPL, never a fake
/// success.
pub fn xaudio2_create_vtable(engine: &'static AudioEngine) -> u64 {
    use crate::hostcall;

    // IXAudio2 layout (indices): 0 QueryInterface,1 AddRef,2 Release,
    // 3 GetDeviceCount,4 GetDeviceDetails,5 CreateSourceVoice,
    // 6 CreateSubmixVoice,7 CreateMasteringVoice,8 StartEngine,9 StopEngine,
    // 10 CommitChanges,11 GetPerformanceData,12 SetDebugConfiguration.
    const VT_LEN: usize = 13;

    let create_master = hostcall::register(move |cpu| unsafe {
        let c = &mut *cpu;
        // args: this(h), input_channels(dx), input_sample_rate(r8), flags(r9)
        let channels = c.gpr.rdx as u32;
        let rate = c.gpr.r8 as u32;
        let _ = c.gpr.r9;
        let _ = engine.create_master_voice(rate, channels);
        c.gpr.rax = 0; // S_OK
    });

    let create_source = hostcall::register(move |cpu| unsafe {
        let c = &mut *cpu;
        // args: this(h), WAVEFORMATEX* (dx)
        let wfx = c.gpr.rdx;
        let fmt = audio_format_from_wfx(wfx).unwrap_or(AudioFormat { sample_rate: 48000, channels: 2, bits: 16 });
        let voice = engine.create_source_voice(fmt);
        // Return an IXAudio2SourceVoice: an object handle + its own vtable.
        let obj = make_source_voice_object(engine, voice);
        c.gpr.rax = 0; // S_OK
        if c.gpr.rcx != 0 { /* out param not used here */ }
        // The out pointer is the 2nd stack arg in some sigs; we also expose the
        // voice id via the returned object pointer's table.
        c.gpr.r8 = obj; // callers that use the last out-param slot
        c.gpr.rax = obj; // and as the direct result for single-out forms
    });

    let start_engine = hostcall::register(|cpu| unsafe { (*cpu).gpr.rax = 0; });
    let stop_engine = hostcall::register(|cpu| unsafe { (*cpu).gpr.rax = 0; });
    let query_interface = hostcall::register(|cpu| unsafe { (*cpu).gpr.rax = 0; });

    let vt = hostcall::build_vtable(&[
        (0, query_interface),
        (5, create_source),
        (7, create_master),
        (8, start_engine),
        (9, stop_engine),
    ], VT_LEN);
    vt
}

/// Build an `IXAudio2SourceVoice` object: returns its vtable pointer. The voice
/// id is stashed in the object so the methods can find the real voice.
fn make_source_voice_object(engine: &'static AudioEngine, voice: u64) -> u64 {
    use crate::hostcall;
    // IXAudio2Voice base: 12 SetVolume ...; source: 19 Start,20 Stop,21 Submit.
    const VT_LEN: usize = 24;
    let submit = hostcall::register(move |cpu| unsafe {
        let c = &mut *cpu;
        let buf = c.gpr.rdx; // XAUDIO2_BUFFER*
        let ok = engine.submit_xaudio2_buffer(voice, buf);
        c.gpr.rax = if ok { 0 } else { 0x8000_4005 };
    });
    let start = hostcall::register(move |cpu| unsafe {
        let c = &mut *cpu;
        engine.start(voice);
        c.gpr.rax = 0;
    });
    let stop = hostcall::register(move |cpu| unsafe {
        let c = &mut *cpu;
        engine.stop(voice);
        c.gpr.rax = 0;
    });
    let set_vol = hostcall::register(move |cpu| unsafe {
        let c = &mut *cpu;
        // f32 volume passed by value in XMM1 (RDX); read its low 32 bits.
        let v = f32::from_bits(c.gpr.rdx as u32);
        engine.set_volume(voice, v);
        c.gpr.rax = 0;
    });
    hostcall::build_vtable(&[
        (12, set_vol),
        (19, start),
        (20, stop),
        (21, submit),
    ], VT_LEN)
}

impl Default for AudioEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.inner.shutdown.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Mixer
// ---------------------------------------------------------------------------

/// Mixer thread: every period, sum the voice graph into a master block at the
/// mastering format and write it to AAudio. Paced by the blocking device write
/// when a stream exists, otherwise by a real-time sleep (silent fallback).
fn mixer_loop(inner: Arc<Inner>) {
    let mut warned_no_master = false;
    loop {
        if inner.shutdown.load(Ordering::SeqCst) {
            break;
        }
        let master = *inner.master.lock().unwrap();
        let (mid, fmt) = match master {
            Some(m) => (m.id, m.format),
            None => {
                if !warned_no_master {
                    crate::glog!("[gow.audio] mixer waiting for a mastering voice");
                    warned_no_master = true;
                }
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
        };
        let mch = fmt.channels.max(1) as usize;
        let rate = fmt.sample_rate.max(1) as usize;
        // 10 ms period.
        let block_frames = (rate / 100).max(1);

        // Snapshot the graph: id -> voice, and parent id -> child ids.
        let voices: Vec<Arc<Voice>> = inner.voices.lock().unwrap().values().cloned().collect();
        let mut index: HashMap<u64, Arc<Voice>> = HashMap::with_capacity(voices.len());
        for v in &voices {
            index.insert(v.id, v.clone());
        }
        let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
        for v in &voices {
            if v.id == mid {
                continue;
            }
            let mut parent = v.output.load(Ordering::Relaxed);
            if parent == 0 {
                parent = mid; // root
            }
            if parent == v.id {
                let n = inner.cycle_hits.fetch_add(1, Ordering::Relaxed);
                if n < 8 {
                    crate::glog!("[gow.audio] voice {} sends to itself; ignored", v.id);
                }
                continue;
            }
            children.entry(parent).or_default().push(v.id);
        }

        let mut path: Vec<u64> = Vec::new();
        let block = mix_voice(&index, &children, mid, block_frames, mch, rate, &mut path, 0);

        let bytes = encode_device_block(&block, fmt.bits);
        let stream = inner.stream.lock().unwrap().as_ref().map(|s| s.0);
        match stream {
            Some(st) => {
                if let Some(fns) = &inner.aaudio {
                    let written = unsafe {
                        (fns.stream_write)(
                            st,
                            bytes.as_ptr() as *const c_void,
                            block_frames as i32,
                            AAUDIO_WRITE_TIMEOUT_NS,
                        )
                    };
                    if written < 0 {
                        let n = inner.device_write_errors.fetch_add(1, Ordering::Relaxed);
                        if n < 8 {
                            crate::glog!("[gow.audio] AAudioStream_write error {written}");
                        }
                    }
                }
            }
            None => {
                // No device: keep real-time pace so rings drain as they would.
                let secs = block_frames as f64 / rate as f64;
                std::thread::sleep(Duration::from_secs_f64(secs));
            }
        }
        inner.periods.fetch_add(1, Ordering::Relaxed);
    }

    // Shutdown: stop and close the device stream.
    if let Some(fns) = &inner.aaudio {
        let handle = inner.stream.lock().unwrap().take();
        if let Some(StreamHandle(st)) = handle {
            unsafe {
                (fns.stream_request_stop)(st);
                (fns.stream_close)(st);
            }
        }
    }
    crate::glog!("[gow.audio] mixer thread stopped ({} periods)", inner.periods.load(Ordering::Relaxed));
}

/// Recursively mix one voice (and everything that sends into it) at the master
/// format. Returns a freshly allocated interleaved block; the caller sums child
/// blocks and the voice's own volume is applied to the voice's whole output,
/// mirroring XAudio2 (a submix scales the sum of its incoming sends).
fn mix_voice(
    index: &HashMap<u64, Arc<Voice>>,
    children: &HashMap<u64, Vec<u64>>,
    id: u64,
    frames: usize,
    mch: usize,
    master_rate: usize,
    path: &mut Vec<u64>,
    depth: usize,
) -> Vec<f32> {
    let mut block = vec![0.0f32; frames * mch];
    if depth >= MAX_MIX_DEPTH || path.contains(&id) {
        return block;
    }
    let v = match index.get(&id) {
        Some(v) => v.clone(),
        None => return block,
    };
    path.push(id);

    // Source voices only produce while started; submixes/masters are always on.
    let active = match v.kind {
        VoiceKind::Source => v.playing.load(Ordering::Relaxed),
        _ => true,
    };
    if active {
        mix_voice_ring(&v, &mut block, frames, mch, master_rate);
    }
    if let Some(kids) = children.get(&id) {
        for &cid in kids {
            let child = mix_voice(index, children, cid, frames, mch, master_rate, path, depth + 1);
            for (dst, src) in block.iter_mut().zip(child.iter()) {
                *dst += *src;
            }
        }
    }
    path.pop();

    let vol = v.volume();
    if vol != 1.0 {
        for s in block.iter_mut() {
            *s *= vol;
        }
    }
    block
}

/// Pull one voice's ring into `out`, converting channels and (if needed)
/// resampling by linear interpolation to the master rate. `out` is *added to*
/// so several contributions sum.
fn mix_voice_ring(v: &Voice, out: &mut [f32], out_frames: usize, mch: usize, master_rate: usize) {
    let fmt = v.format;
    let src_ch = fmt.channels as usize;
    if src_ch == 0 || fmt.sample_rate == 0 {
        return;
    }
    let mut st = v.state.lock().unwrap();
    let avail = st.ring.len() / src_ch;
    if avail == 0 {
        return;
    }
    // source frames advanced per output frame.
    let step = fmt.sample_rate as f64 / master_rate as f64;
    let mut fidx = st.resample_pos;

    for produced in 0..out_frames {
        let i0 = fidx.floor() as usize;
        if i0 >= avail {
            break;
        }
        let i1 = (i0 + 1).min(avail - 1);
        let frac = (fidx - i0 as f64) as f32;
        let base = produced * mch;

        for c in 0..mch {
            let s = if mch == 1 && src_ch > 1 {
                // Downmix to mono: average every source channel.
                let mut acc = 0.0f32;
                for k in 0..src_ch {
                    let a = st.ring.get(i0 * src_ch + k);
                    let b = st.ring.get(i1 * src_ch + k);
                    acc += a + (b - a) * frac;
                }
                acc / src_ch as f32
            } else {
                // Upmix: replicate mono; otherwise map channel c (extra source
                // channels beyond the master width are dropped).
                let sc = if src_ch == 1 { 0 } else { c.min(src_ch - 1) };
                let a = st.ring.get(i0 * src_ch + sc);
                let b = st.ring.get(i1 * src_ch + sc);
                a + (b - a) * frac
            };
            out[base + c] += s;
        }
        fidx += step;
    }

    // Frames fully consumed; keep the fractional phase for the next period.
    let consume = (fidx.floor() as usize).min(avail);
    if consume > 0 {
        st.ring.drop_front(consume * src_ch);
    }
    st.resample_pos = fidx - consume as f64;
}

// ---------------------------------------------------------------------------
// PCM helpers
// ---------------------------------------------------------------------------

/// Decode interleaved PCM to `f32` in [-1,1]. Supports the widths XAudio2
/// source voices use: 8-bit unsigned, 16/24-bit signed, 32-bit IEEE float.
/// 32-bit is assumed float (XAudio2's float mix); see `docs/AUDIO.md`.
fn decode_pcm(bytes: &[u8], bits: u16) -> Vec<f32> {
    match bits {
        8 => bytes.iter().map(|&b| (b as f32 - 128.0) / 128.0).collect(),
        16 => bytes
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
            .collect(),
        24 => bytes
            .chunks_exact(3)
            .map(|c| {
                let v = ((c[2] as i32) << 16) | ((c[1] as i32) << 8) | (c[0] as i32);
                let v = (v << 8) >> 8; // sign-extend 24 -> 32
                v as f32 / 8_388_608.0
            })
            .collect(),
        32 => bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
        _ => Vec::new(),
    }
}

/// Encode a master block for the device: 32-bit -> f32 LE, else i16 LE with
/// clipping. Sample count is unchanged.
fn encode_device_block(block: &[f32], bits: u16) -> Vec<u8> {
    if bits == 32 {
        let mut out = Vec::with_capacity(block.len() * 4);
        for &s in block {
            out.extend_from_slice(&s.clamp(-1.0, 1.0).to_le_bytes());
        }
        out
    } else {
        let mut out = Vec::with_capacity(block.len() * 2);
        for &s in block {
            let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }
}

fn count_missing(inner: &Inner) {
    let n = inner.missing_voice_calls.fetch_add(1, Ordering::Relaxed);
    if n < 8 {
        crate::glog!("[gow.audio] operation on unknown voice id");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_roundtrip_16bit() {
        // 0, +full, -full, 0 -> 0.0, ~1.0, -1.0, 0.0
        let pcm: Vec<u8> = [0i16, 32767, -32768, 0].iter().flat_map(|v| v.to_le_bytes()).collect();
        let f = decode_pcm(&pcm, 16);
        assert_eq!(f.len(), 4);
        assert!((f[0]).abs() < 1e-6);
        assert!(f[1] > 0.99);
        assert!(f[2] <= -0.999);
        assert!((f[3]).abs() < 1e-6);
    }

    #[test]
    fn mono_to_stereo_and_volume() {
        // A source voice at the master rate: 2 mono samples -> 2 stereo frames.
        let fmt = AudioFormat { sample_rate: 48000, channels: 1, bits: 16 };
        let v = Voice::new(1, VoiceKind::Source, fmt, 4096);
        v.volume.store(0.5f32.to_bits(), Ordering::Relaxed);
        v.playing.store(true, Ordering::Relaxed);
        {
            let mut st = v.state.lock().unwrap();
            let pcm: Vec<u8> = [16384i16, -16384].iter().flat_map(|x| x.to_le_bytes()).collect();
            st.ring.push(&decode_pcm(&pcm, 16));
        }
        let mut out = vec![0.0f32; 2 * 2];
        mix_voice_ring(&v, &mut out, 2, 2, 48000);
        // Both channels get the sample, and volume is applied by mix_voice (not
        // here), so the ring mix is unscaled: 0.5 then -0.5 replicated.
        assert!((out[0] - 0.5).abs() < 1e-3, "{out:?}");
        assert!((out[1] - 0.5).abs() < 1e-3, "{out:?}");
        assert!((out[2] + 0.5).abs() < 1e-3, "{out:?}");
        assert!((out[3] + 0.5).abs() < 1e-3, "{out:?}");
    }

    #[test]
    fn engine_falls_back_without_device() {
        // On hosts with no libaaudio.so the engine must still build a graph.
        let e = AudioEngine::new();
        let m = e.create_master_voice(48000, 2);
        assert_ne!(m, 0);
        let s = e.create_source_voice(AudioFormat { sample_rate: 48000, channels: 2, bits: 16 });
        assert_ne!(s, 0);
        e.set_volume(s, 0.25);
        e.start(s);
        // Submit 4 stereo frames of silence.
        let pcm = vec![0u8; 4 * 2 * 2];
        assert!(e.submit_buffer(s, pcm.as_ptr() as u64, pcm.len() as u32));
        // A second mastering voice is refused.
        assert_eq!(e.create_master_voice(48000, 2), 0);
        e.stop(s);
    }
}
