# Audio layer (XAudio2 -> AAudio)

God of War (2018) PC mixes its audio with **XAudio2**. The recompiled code
reaches XAudio2 through its IAT (`XAudio2Create`, and the COM objects it
returns), so the runtime must provide those objects and do the actual mixing on
Android. This layer lives in `runtime/src/audio.rs` and targets **AAudio**
(the NDK C API for audio) — chosen over Oboe because Oboe is a C++ library while
AAudio is a plain C API we can bind with `dlopen`/`dlsym`, exactly like
`vk.rs` binds Vulkan and `graphics.rs` binds the DXVK backend.

## Design

```
 game thread (recompiled code)              mixer thread ("gow-audio")
 ----------------------------               --------------------------
 submit_buffer(voice, pcm, n)  --decode-->  per-voice ring buffer (f32)
   (PCM the game submits)                    |
                                             v   channel map + linear resample
                                     recursive graph mix (sources -> submix -> master)
                                             |
                                             v   to device format (i16 / f32)
                                     AAudioStream_write(libaaudio.so)
```

- `AudioEngine` owns the mixing graph: a map of `voice id -> Voice`.
- Each **source voice** has a fixed-capacity ring of `f32` samples. The game
  thread decodes submitted PCM into the ring; the mixer thread drains it.
- **Submix voices** re-scale and sum whatever sends into them; the **mastering
  voice** is the root of the graph and corresponds to the device.
- The mixer runs one period (10 ms) per iteration and either blocks on the
  AAudio write (real device) or sleeps a real-time slice (silent fallback).
- Host-side mixing is real: only PCM the game submits is ever played. Nothing is
  fabricated. Unknown ids, malformed buffers and unsupported paths **log and
  return failure** (`false` / `0`), consistent with D005/D012.

### Public surface

| Method | XAudio2 equivalent |
| --- | --- |
| `AudioEngine::new()` | `XAudio2Create` setup |
| `create_master_voice(rate, channels) -> u64` | `IXAudio2::CreateMasteringVoice` |
| `create_source_voice(AudioFormat) -> u64` | `IXAudio2::CreateSourceVoice` |
| `create_submix_voice(ch, rate, output) -> u64` | `IXAudio2::CreateSubmixVoice` |
| `submit_buffer(v, ptr, bytes) -> bool` | `IXAudio2SourceVoice::SubmitSourceBuffer` |
| `submit_xaudio2_buffer(v, xaudio2_buffer_ptr)` | reads a guest `XAUDIO2_BUFFER` |
| `set_volume(v, f32)` | `IXAudio2Voice::SetVolume` |
| `set_output(v, target)` | `IXAudio2Voice::SetOutputVoices` (single send) |
| `start(v)` / `stop(v)` | `IXAudio2SourceVoice::Start` / `Stop` |
| `flush_source_buffers(v)` | `IXAudio2SourceVoice::FlushSourceBuffers` |
| `output_available()`, `stats()` | diagnostics (no fabricated numbers) |

`AudioFormat { sample_rate: u32, channels: u16, bits: u16 }` is the subset of
`WAVEFORMATEX` the mixer needs. `audio_format_from_wfx(ptr)` reads a packed
`WAVEFORMATEX` from guest memory (guest pointers are host pointers here, D004).

## XAudio2 -> AAudio mapping

| XAudio2 | Here | AAudio |
| --- | --- | --- |
| `IXAudio2` mastering voice / device | `create_master_voice` | `AAudio_createStreamBuilder` + `setFormat/setChannelCount/setSampleRate` + `openStream` + `requestStart` |
| mix quantum (~10 ms) | mixer period | `AAudioStream_write` (blocking, 100 ms timeout) |
| `SubmitSourceBuffer` PCM | `submit_buffer` -> ring | decoded in the mixer, written to the stream |
| `SetVolume` | `set_volume` (atomic) | applied per-voice during the mix |
| `Start` / `Stop` | `start` / `stop` flags | source voices only produce while started |
| `SetOutputVoices` (1 send) | `set_output` | edge in the mixing graph |
| device teardown | `Drop for AudioEngine` | `AAudioStream_requestStop` + `AAudioStream_close` |

The mix is always done in `f32`. The device stream is opened as 16-bit PCM
(`AAUDIO_FORMAT_PCM_I16`, broadly supported); a master voice created with 32-bit
samples opens a float stream (`AAUDIO_FORMAT_PCM_FLOAT`). Submitted PCM is
decoded for the voice's own width (8-bit unsigned, 16/24-bit signed, 32-bit
assumed float — XAudio2's internal mix format). Channel counts are mapped: mono
is replicated to every output channel, stereo/multi maps channel-for-channel,
extra source channels are dropped, and multi-to-mono averages.

## COM ABI

The game calls XAudio2 as COM: the function pointer is
`*(*(void***)pThis + index)`, with `pThis` in RCX per the Microsoft x64 ABI.
Indices are absolute in each vtable (derived interfaces repeat the base
methods). The full tables are documented at the top of `audio.rs`; the most
relevant indices are also exported as constants (`IXAUDIO2_CREATE_MASTERING_VOICE`
= 7, `IXAUDIO2SOURCEVOICE_SUBMIT_SOURCE_BUFFER` = 21, ...) for the `win32.rs`
wiring.

## AAudio binding and the no-op fallback

`libaaudio.so` is opened with `dlopen(RTLD_NOW)`; each entry point is resolved
with `dlsym`. If the library is missing (dev host, or an environment without
AAudio) or a symbol is absent, `AudioEngine` logs via `crate::glog!`, keeps its
full graph (voices, rings, mixer all work), and simply has no device sink.
`output_available()` reports this; nothing is faked.

## Implemented vs missing

**Implemented**

- Engine/graph lifecycle, ring buffers, decode of 8/16/24/32-bit PCM.
- Mastering/source/submix voices; volume; start/stop; flush; single output send.
- Real host-side mixing: channel mapping, volume application per voice, linear
  resampling when a voice rate differs from the master rate.
- AAudio output with a real-time paced silent fallback when no device exists.
- Overflow is handled by dropping the oldest quarter of a ring (counted) so
  latency cannot grow without bound.

**Missing (deliberate; return failure, never fake)**

- `CreateAudioReverb` / `CreateAudioVolumeMeter`: these return `IXAPO`/
  `IXAPOFactories` COM objects. Not implemented — callers should fail and log.
- Effects chains / `SetEffectChain` / `EnableEffect` / filters / output matrices.
- Per-buffer loop regions and `XAUDIO2_END_OF_STREAM` callback semantics (the
  flag is logged; `pBufferWMA` and compression are ignored — PCM only).
- `SetFrequencyRatio` / `SetSourceSampleRate` (the ring resampler exists but is
  driven by the voice format, not by a ratio call).
- Voice callbacks (`IXAudio2VoiceCallback`) and
  `GetState`/`GetVoiceDetails`/`GetPerformanceData`.
- Multi-device enumeration (`GetDeviceCount`/`GetDeviceDetails`) and non-default
  mastering devices.

## Wiring

Add `pub mod audio;` to `runtime/src/lib.rs`. `win32.rs` should then resolve the
`xaudio2_*.dll` IAT slots: `XAudio2Create` creates an `AudioEngine` (store it in
the resolver) and returns a fabricated-but-honest `IXAudio2*` whose vtable slots
route to the `AudioEngine` methods above by the documented indices.

## Tests

`runtime/src/audio.rs` has unit tests for 16-bit decode, mono-to-stereo mixing
with volume, and the no-device fallback path. They run without a device:

```
cargo test -p gow-runtime audio
```
