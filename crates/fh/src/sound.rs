//! Success notification sound.
//!
//! When a remote run finishes successfully, the client plays a short clip of
//! the project's pronunciation. This is cosmetic: every failure mode here is
//! swallowed so audio can never turn a passing build into a failing one.
//!
//! # Output backends
//!
//! - **Linux**: a minimal PulseAudio *native protocol* client spoken directly
//!   over the server's unix socket. This deliberately avoids linking `libasound`
//!   (via `cpal`/`rodio`), because a linked ALSA library would break the fully
//!   static musl release binary. It is marked experimental and gated at runtime
//!   by [`SoundMode::RawPulse`].
//! - **Other platforms**: `rodio`, which links the OS audio *library*
//!   (CoreAudio/WASAPI). Linking a system library keeps the "never execute a
//!   system binary" rule intact; this crate shells out to nothing.
//!
//! The mp3 is embedded with `include_bytes!` and lives inside the crate
//! directory so that `cargo package`/`cargo install` ship it.

use std::time::Duration;

/// The project's pronunciation, embedded into the binary as 16-bit PCM WAV.
const CLIP: &[u8] = include_bytes!("../assets/pronunciation.wav");

/// Which output backend (if any) the client should attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundMode {
    /// No sound.
    Off,
    /// Platform audio library (`rodio`); used on non-Linux targets.
    Platform,
    /// Raw PulseAudio native-protocol socket; used on Linux.
    RawPulse,
}

/// Decide which backend to use from the environment, defaulting to `Off` for
/// the raw socket (experimental) but `Platform` elsewhere.
///
/// CI and other non-interactive environments are always silenced: a CI run
/// that makes noise is a bug, not a feature.
///
/// `flag_off` mirrors the `--no-sound` CLI flag and wins over everything.
pub fn mode_from_env(flag_off: bool) -> SoundMode {
    if flag_off || ci_detected() {
        return SoundMode::Off;
    }
    match std::env::var("FARHAND_SOUND").as_deref() {
        Ok("off") | Ok("0") | Ok("false") => SoundMode::Off,
        Ok("raw") => SoundMode::RawPulse,
        Ok("on") | Ok("1") | Ok("true") | Ok("platform") => SoundMode::Platform,
        // Default: enable the platform library on non-Linux, but on Linux keep
        // the raw (experimental) path opt-in.
        _ => {
            if cfg!(target_os = "linux") {
                SoundMode::Off
            } else {
                SoundMode::Platform
            }
        }
    }
}

fn ci_detected() -> bool {
    for key in [
        "CI",
        "CONTINUOUS_INTEGRATION",
        "GITHUB_ACTIONS",
        "BUILDKITE",
    ] {
        if let Ok(v) = std::env::var(key) {
            if !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false") {
                return true;
            }
        }
    }
    false
}

/// Parse the embedded clip into interleaved `i16` PCM.
///
/// The clip ships as 16-bit PCM WAV rather than MP3 so that playback needs no
/// decoder at all. That is a deliberate trade: every pure-Rust MP3 decoder we
/// could reach either drags in copyleft (symphonia is MPL-2.0) or, in
/// minimp3's case, a `slice-ring-buffer` with unpatched double-free advisories
/// (RUSTSEC-2025-0044). Reading a RIFF header costs us a few dozen lines and
/// removes an entire dependency from a tool that otherwise has a strict licence
/// and advisory policy.
fn decode_clip() -> Option<(Vec<i16>, u32, u16)> {
    let bytes = CLIP;
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }

    let mut sample_rate = 0u32;
    let mut channels = 0u16;
    let mut bits = 0u16;
    let mut audio_format = 0u16;
    let mut data: Option<&[u8]> = None;

    // Walk the RIFF chunk list; `fmt ` and `data` are what we need, and any
    // other chunk (LIST, fact, …) is skipped by its declared size.
    let mut pos = 12usize;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        let body_start = pos + 8;
        let body_end = body_start.checked_add(size)?;
        if body_end > bytes.len() {
            return None;
        }
        match id {
            b"fmt " if body_end - body_start >= 16 => {
                let b = &bytes[body_start..body_end];
                audio_format = u16::from_le_bytes([b[0], b[1]]);
                channels = u16::from_le_bytes([b[2], b[3]]);
                sample_rate = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
                bits = u16::from_le_bytes([b[14], b[15]]);
            }
            b"data" => data = Some(&bytes[body_start..body_end]),
            _ => {}
        }
        // Chunks are word-aligned: an odd size is followed by a pad byte.
        pos = body_end + (size & 1);
    }

    // Only uncompressed 16-bit PCM is supported; anything else would need a
    // decoder, which is exactly what this format choice avoids.
    if audio_format != 1 || bits != 16 || channels == 0 || sample_rate == 0 {
        return None;
    }
    let raw = data?;
    if raw.len() < 2 || raw.len() % 2 != 0 {
        return None;
    }
    let samples: Vec<i16> = raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| i16::from_le_bytes(*c))
        .collect();
    if samples.is_empty() {
        return None;
    }
    Some((samples, sample_rate, channels))
}

/// The duration of the decoded clip, used to size a timeout so a wedged audio
/// backend can never hang the client on the success path.
fn clip_duration(samples: &[i16], sample_rate: u32, channels: u16) -> Duration {
    let frames = samples.len() / channels.max(1) as usize;
    let secs = frames as f64 / f64::from(sample_rate.max(1));
    // The clip is a couple of seconds; the ceiling is just a safety net so a
    // pathological length can never hold the process open.
    Duration::from_secs_f64((secs + 2.0).min(30.0))
}

/// Play the success sound, best-effort. Returns immediately and never panics.
pub fn play_success(flag_off: bool) {
    let mode = mode_from_env(flag_off);
    if mode == SoundMode::Off {
        return;
    }
    let Some((samples, rate, channels)) = decode_clip() else {
        return;
    };
    let budget = clip_duration(&samples, rate, channels);

    match mode {
        SoundMode::Off => {}
        #[cfg(not(target_os = "linux"))]
        SoundMode::Platform | SoundMode::RawPulse => play_rodio(&samples, rate, channels, budget),
        #[cfg(target_os = "linux")]
        SoundMode::Platform => {}
        #[cfg(target_os = "linux")]
        SoundMode::RawPulse => play_pulse(&samples, rate, channels, budget),
    }
}

/// Play via `rodio` (non-Linux: CoreAudio/WASAPI). On Linux `rodio` is not a
/// dependency, so the raw-socket path is used instead.
#[cfg(not(target_os = "linux"))]
fn play_rodio(samples: &[i16], sample_rate: u32, channels: u16, _budget: Duration) {
    use rodio::buffer::SamplesBuffer;
    use rodio::{OutputStreamBuilder, Sink};

    let Ok(stream) = OutputStreamBuilder::open_default_stream() else {
        return;
    };
    // rodio plays `f32`; minimp3 hands us interleaved `i16`. Scale to [-1, 1].
    let pcm: Vec<f32> = samples.iter().map(|s| *s as f32 / 32_768.0).collect();
    let source = SamplesBuffer::new(channels.max(1), sample_rate.max(1), pcm);
    // `stream` must outlive the sink: the mixer is borrowed from it.
    let sink = Sink::connect_new(stream.mixer());
    sink.append(source);
    // Block until the clip finishes, so it is actually heard before we exit.
    sink.sleep_until_end();
}

// ---------------------------------------------------------------------------
// Linux: PulseAudio native protocol client
// ---------------------------------------------------------------------------
#[cfg(target_os = "linux")]
mod pulse {
    //! A tiny, minimal PulseAudio native-protocol client.
    //!
    //! The wire format was derived from PulseAudio's own source:
    //! - Descriptor frame: five big-endian `u32`s —
    //!   `[len, channel, offset_hi, offset_lo, flags]`. For a control packet the
    //!   channel is `0xFFFF_FFFF`; for a stream memblock it is the stream's
    //!   channel id.
    //! - Payload ("tagstruct"): a command `u32`, a request `tag` `u32`, then a
    //!   sequence of type-tagged fields (`'L'` u32, `'t'` string, `'a'`
    //!   sample_spec, `'m'` channel_map, `'1'/'0'` boolean, `'x'` arbitrary,
    //!   `'P'` proplist).
    //!
    //! This is intentionally small: it connects, authenticates with the
    //! per-user cookie, creates a playback stream, writes the PCM, drains, and
    //! tears the stream down.

    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;
    use std::time::Duration;

    const PA_SAMPLE_S16LE: u8 = 3;
    const PA_PROTOCOL_VERSION: u32 = 35;

    const CMD_CREATE_PLAYBACK_STREAM: u32 = 3;
    const CMD_DELETE_PLAYBACK_STREAM: u32 = 4;
    const CMD_AUTH: u32 = 8;
    const CMD_DRAIN_PLAYBACK_STREAM: u32 = 12;

    const INVALID_INDEX: u32 = 0xFFFF_FFFF;

    // channel map positions
    const CHNL_MONO: u8 = 0;
    const CHNL_FRONT_LEFT: u8 = 1;
    const CHNL_FRONT_RIGHT: u8 = 2;

    const XDG_RUNTIME: &str = "XDG_RUNTIME_DIR";

    /// Write a big-endian u32.
    fn put_u32(out: &mut Vec<u8>, v: u32) {
        out.extend_from_slice(&v.to_be_bytes());
    }
    fn put_string_null(out: &mut Vec<u8>) {
        out.push(b'N');
    }
    fn put_bool(out: &mut Vec<u8>, b: bool) {
        out.push(if b { b'1' } else { b'0' });
    }
    fn put_sample_spec(out: &mut Vec<u8>, format: u8, channels: u8, rate: u32) {
        // PA_TAG_SAMPLE_SPEC 'a': format u8, channels u8, rate u32
        out.push(b'a');
        out.push(format);
        out.push(channels);
        put_u32(out, rate);
    }
    fn put_channel_map(out: &mut Vec<u8>, map: &[u8]) {
        // PA_TAG_CHANNEL_MAP 'm': count u8, then one u8 per position.
        out.push(b'm');
        out.push(map.len() as u8);
        out.extend_from_slice(map);
    }
    fn put_arbitrary(out: &mut Vec<u8>, data: &[u8]) {
        // PA_TAG_ARBITRARY 'x': length u32 then raw bytes.
        out.push(b'x');
        put_u32(out, data.len() as u32);
        out.extend_from_slice(data);
    }

    /// Assemble the 20-byte descriptor + payload into one buffer.
    pub(crate) fn frame(payload: &[u8], channel: u32, offset: u64, flags: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity(20 + payload.len());
        put_u32(&mut out, payload.len() as u32); // descriptor[0] = payload length
        put_u32(&mut out, channel); // descriptor[1] = channel (-1 for control)
        put_u32(&mut out, (offset >> 32) as u32); // descriptor[2] = offset hi
        put_u32(&mut out, offset as u32); // descriptor[3] = offset lo
        put_u32(&mut out, flags); // descriptor[4] = flags (seek mode / bitfield)
        out.extend_from_slice(payload);
        out
    }

    /// Read exactly `n` bytes, honouring an overall deadline.
    fn read_exact_timeout(
        sock: &mut UnixStream,
        n: usize,
        deadline: std::time::Instant,
    ) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; n];
        let mut got = 0usize;
        while got < n {
            if std::time::Instant::now() >= deadline {
                return None;
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            sock.set_read_timeout(Some(remaining)).ok()?;
            match sock.read(&mut buf[got..]) {
                Ok(0) => return None, // EOF
                Ok(k) => got += k,
                Err(_) => return None,
            }
        }
        Some(buf)
    }

    fn read_u32_be(b: &[u8]) -> u32 {
        u32::from_be_bytes([b[0], b[1], b[2], b[3]])
    }

    /// Locate the cookie file (256-byte auth secret).
    pub(crate) fn cookie_path() -> PathBuf {
        if let Ok(base) = std::env::var("XDG_CONFIG_HOME") {
            let p = PathBuf::from(base).join("pulse").join("cookie");
            if p.exists() {
                return p;
            }
        }
        if let Ok(home) = std::env::var("HOME") {
            let p = PathBuf::from(home)
                .join(".config")
                .join("pulse")
                .join("cookie");
            if p.exists() {
                return p;
            }
        }
        // Common default even without XDG_CONFIG_HOME.
        PathBuf::from("/root/.config/pulse/cookie")
    }

    /// Locate the native socket.
    ///
    /// The session socket lives under `XDG_RUNTIME_DIR`; that is the only path
    /// we consult, so the crate stays free of `unsafe` (`getuid` would need an
    /// `extern "C"` shim). A missing/unset `XDG_RUNTIME_DIR` simply means no
    /// sound.
    pub(crate) fn socket_path() -> Option<PathBuf> {
        let dir = std::env::var_os(XDG_RUNTIME)?;
        let p = PathBuf::from(dir).join("pulse").join("native");
        if p.exists() {
            Some(p)
        } else {
            None
        }
    }

    /// Authenticate with the server and return the negotiated protocol version.
    ///
    /// The reply is a tagstruct: `L <cmd=REPLY>`, `L <tag>`, then body fields. The
    /// first body field is the server's protocol version.
    pub(crate) fn auth(sock: &mut UnixStream, deadline: std::time::Instant) -> Option<u32> {
        let mut cookie = vec![0u8; 256];
        std::fs::File::open(cookie_path())
            .ok()?
            .read_exact(&mut cookie)
            .ok()?;

        // payload: putu32(command) putu32(tag) putu32(version) arbitrary(cookie)
        let mut payload = Vec::new();
        put_u32(&mut payload, CMD_AUTH);
        put_u32(&mut payload, 0); // tag
        put_u32(&mut payload, PA_PROTOCOL_VERSION);
        put_arbitrary(&mut payload, &cookie);

        let buf = frame(&payload, INVALID_INDEX, 0, 0);
        sock.write_all(&buf).ok()?;

        let reply = read_reply_payload(sock, deadline)?;
        // The AUTH reply is a tagstruct of `L`(u32) fields:
        //   'L' tag, 'L' client_index, 'L' result ...
        // It does not inline the command the way a request does, so we accept
        // any reply that parses as a sequence of u32 fields and treat the
        // connection as authenticated.
        if reply.len() >= 15 && reply[0] == b'L' {
            // second field's value = client index
            Some(read_u32_be(&reply[6..10]))
        } else {
            None
        }
    }

    /// Read one descriptor + payload from the socket.
    pub(crate) fn read_reply_payload(
        sock: &mut UnixStream,
        deadline: std::time::Instant,
    ) -> Option<Vec<u8>> {
        let desc = read_exact_timeout(sock, 20, deadline)?;
        let len = read_u32_be(&desc[0..4]) as usize;
        if len == 0 || len > 1 << 20 {
            return None;
        }
        read_exact_timeout(sock, len, deadline)
    }

    /// Create a playback stream; return its channel id.
    pub(crate) fn create_stream(
        sock: &mut UnixStream,
        tag: u32,
        rate: u32,
        channels: u16,
        deadline: std::time::Instant,
    ) -> Option<u32> {
        let ch = channels as u8;
        let map: Vec<u8> = match channels {
            1 => vec![CHNL_MONO],
            2 => vec![CHNL_FRONT_LEFT, CHNL_FRONT_RIGHT],
            _ => (0..ch).collect(),
        };

        let mut payload = Vec::new();
        put_u32(&mut payload, CMD_CREATE_PLAYBACK_STREAM);
        put_u32(&mut payload, tag);
        put_sample_spec(&mut payload, PA_SAMPLE_S16LE, ch, rate);
        put_channel_map(&mut payload, &map);
        put_u32(&mut payload, INVALID_INDEX); // sink index (auto)
        put_string_null(&mut payload); // device name (default)
        put_u32(&mut payload, 0); // maxlength (unused)
        put_bool(&mut payload, true); // corked — fill buffer before uncorking
                                      // playback-specific
        put_u32(&mut payload, 0); // tlength
        put_u32(&mut payload, 0); // prebuf
        put_u32(&mut payload, 0); // minreq
        put_u32(&mut payload, 0); // syncid
                                  // cvolume: 'v' channels u8 then u32 per channel (100% = PA_VOLUME_NORM)
        payload.push(b'v');
        payload.push(ch);
        for _ in 0..ch {
            put_u32(&mut payload, 0x10000); // 100%
        }
        // version >= 12: seven booleans
        put_bool(&mut payload, false); // no_remap_channels
        put_bool(&mut payload, false); // no_remix_channels
        put_bool(&mut payload, false); // fix_format
        put_bool(&mut payload, false); // fix_rate
        put_bool(&mut payload, false); // fix_channels
        put_bool(&mut payload, false); // dont_move
        put_bool(&mut payload, false); // variable_rate
                                       // version >= 13: start_muted (playback), adjust_latency, proplist
        put_bool(&mut payload, false); // start_muted
        put_bool(&mut payload, false); // adjust_latency
        payload.push(b'N'); // empty proplist terminator (PA_TAG_PROPLIST 'P' + one null key is heavy; a bare 'N' is accepted for empty)
                            // version >= 14: volume_set, early_requests
        put_bool(&mut payload, false);
        put_bool(&mut payload, false);
        // version >= 15: start_muted_unmuted, dont_inhibit, fail_on_suspend
        put_bool(&mut payload, false);
        put_bool(&mut payload, false);
        put_bool(&mut payload, false);
        // version >= 17/18: relative_volume, passthrough
        put_bool(&mut payload, false);
        put_bool(&mut payload, false);
        // version >= 21: n_formats u8 = 0
        payload.push(b'B'); // PA_TAG_U8
        payload.push(0); // no explicit formats -> use sample spec

        let buf = frame(&payload, INVALID_INDEX, 0, 0);
        sock.write_all(&buf).ok()?;

        // Reply: 'L' cmd=REPLY, 'L' tag, 'L' channel, 'L' stream_index, ...
        // Each field is `type(1)+u32(4)`; channel is the 3rd field's value.
        let reply = read_reply_payload(sock, deadline)?;
        // Reply fields are `type(1)+u32(4)`. The first field is the channel id.
        if reply.len() >= 10 && reply[0] == b'L' {
            Some(read_u32_be(&reply[1..5]))
        } else {
            None
        }
    }

    /// Write the PCM as memblocks on the stream's channel.
    pub(crate) fn write_pcm(
        sock: &mut UnixStream,
        channel: u32,
        samples: &[i16],
        deadline: std::time::Instant,
    ) -> Option<()> {
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let max_block = 8192usize; // 16 KiB PCM blocks
        let mut offset: u64 = 0;
        for block in bytes.chunks(max_block) {
            if std::time::Instant::now() >= deadline {
                return None;
            }
            // descriptor: channel = stream channel, flags = seek mode. We send
            // the descriptor with the memblock payload inline (no separate
            // PA_TAG_ARBITRARY for stream data — stream data is raw).
            let buf = frame(block, channel, offset, 0);
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            sock.set_write_timeout(Some(remaining)).ok()?;
            sock.write_all(&buf).ok()?;
            offset += block.len() as u64;
        }
        Some(())
    }

    /// Drain the stream and delete it, then close.
    pub(crate) fn finish(
        sock: &mut UnixStream,
        channel: u32,
        tag: u32,
        deadline: std::time::Instant,
    ) {
        // Drain (play out everything) then delete. We ignore replies; the point
        // is to give the server time to render before teardown.
        let mut drain = Vec::new();
        put_u32(&mut drain, CMD_DRAIN_PLAYBACK_STREAM);
        put_u32(&mut drain, tag);
        put_u32(&mut drain, channel);
        let _ = sock.write_all(&frame(&drain, INVALID_INDEX, 0, 0));

        // Give it a moment to render, staying under the overall budget.
        let _ = read_reply_payload(sock, deadline);

        let mut del = Vec::new();
        put_u32(&mut del, CMD_DELETE_PLAYBACK_STREAM);
        put_u32(&mut del, tag + 1);
        put_u32(&mut del, channel);
        let _ = sock.write_all(&frame(&del, INVALID_INDEX, 0, 0));
        let _ = sock.flush();
    }

    /// Entry point: play interleaved `i16` PCM over the native socket.
    pub fn play(samples: &[i16], sample_rate: u32, channels: u16, budget: Duration) {
        let Some(path) = socket_path() else { return };
        let Ok(mut sock) = UnixStream::connect(&path) else {
            return;
        };
        let deadline = std::time::Instant::now() + budget;

        let Some(_) = auth(&mut sock, deadline) else {
            return;
        };
        let Some(channel) = create_stream(&mut sock, 1, sample_rate, channels, deadline) else {
            return;
        };
        if write_pcm(&mut sock, channel, samples, deadline).is_none() {
            return;
        }
        finish(&mut sock, channel, 1, deadline);
    }
}

#[cfg(target_os = "linux")]
fn play_pulse(samples: &[i16], sample_rate: u32, channels: u16, budget: Duration) {
    pulse::play(samples, sample_rate, channels, budget);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_decodes_to_pcm() {
        let (samples, rate, channels) = decode_clip().expect("clip should decode");
        assert!(!samples.is_empty(), "clip should yield samples");
        assert!((8000..=192_000).contains(&rate), "sane sample rate: {rate}");
        assert!(channels == 1 || channels == 2, "mono or stereo: {channels}");
    }

    #[test]
    fn clip_duration_is_bounded() {
        let (samples, rate, channels) = decode_clip().expect("clip should decode");
        let d = clip_duration(&samples, rate, channels);
        assert!(d <= Duration::from_secs(30));
        assert!(d >= Duration::from_millis(100));
    }

    #[test]
    fn ci_is_always_off() {
        std::env::set_var("CI", "1");
        assert_eq!(mode_from_env(false), SoundMode::Off);
        std::env::remove_var("CI");
    }

    #[test]
    fn explicit_off_is_respected() {
        std::env::set_var("FARHAND_SOUND", "off");
        assert_eq!(mode_from_env(false), SoundMode::Off);
        std::env::remove_var("FARHAND_SOUND");
    }

    #[test]
    fn no_sound_flag_overrides_env() {
        std::env::set_var("FARHAND_SOUND", "on");
        assert_eq!(mode_from_env(true), SoundMode::Off);
        std::env::remove_var("FARHAND_SOUND");
    }

    /// Exercises the raw PulseAudio client against a live server. Ignored by
    /// default (no audio server in CI); run with `--ignored` on a desktop.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires a running PulseAudio/PipeWire server"]
    fn raw_pulse_protocol_completes() {
        assert!(pulse::socket_path().is_some(), "native socket should exist");
        assert!(pulse::cookie_path().exists(), "cookie should exist");

        let mut sock =
            std::os::unix::net::UnixStream::connect(pulse::socket_path().expect("socket path"))
                .expect("connect to native socket");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);

        let index = pulse::auth(&mut sock, deadline).expect("auth should succeed");
        eprintln!("auth OK, client index = {index}");

        let (samples, rate, channels) = decode_clip().expect("clip decodes");
        let channel = pulse::create_stream(&mut sock, 1, rate, channels, deadline)
            .expect("create playback stream");
        eprintln!("create OK, stream channel = {channel}");

        pulse::write_pcm(&mut sock, channel, &samples, deadline).expect("write pcm");
        eprintln!("write OK ({} samples)", samples.len());

        pulse::finish(&mut sock, channel, 1, deadline);
        eprintln!("drain/delete OK");
    }
}
