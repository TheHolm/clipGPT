//! Minimal cross-platform beep tone generator, built directly on `cpal`.
//!
//! This exists instead of depending on the `actually_beep` crate because
//! that crate pins `cpal` to `^0.15.3` - a version with no BSD/FreeBSD
//! backend at all (BSD support was only added to `cpal` in a later
//! release). Calling `cpal` directly lets us use a current version that
//! does support FreeBSD, which is a hard-required target for this project.
//!
//! On Linux, `cpal`'s only enabled host is ALSA (see `Cargo.lock`), which
//! in turn is usually routed through `pipewire-alsa` or PulseAudio's ALSA
//! plugin. That routing only starts delivering audio once ALSA's playback
//! buffer has filled past its start threshold. With a large default
//! buffer and a short tone, the stream can be built, played, and then
//! dropped (cutting playback immediately, with no draining) before that
//! threshold is ever reached - silently, since per-callback stream errors
//! were previously discarded. This module works around that by requesting
//! a small buffer, waiting for enough frames to have actually been
//! written before dropping the stream, and surfacing stream errors
//! instead of throwing them away.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SizedSample};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

/// Requested output buffer size, in milliseconds, when the device reports
/// a supported range. A small buffer reaches ALSA's start threshold
/// quickly, so short tones are not dropped before they ever play.
const TARGET_BUFFER_MILLIS: u32 = 20;

/// Fade-in/fade-out duration, in milliseconds, applied at the start and
/// end of the tone to avoid audible clicks now that playback is cut at an
/// exact sample instead of just sleeping past it.
const FADE_MILLIS: u32 = 5;

/// Extra time, beyond the requested tone duration, allowed for the stream
/// to actually finish draining before giving up and dropping it anyway.
/// This bounds the worst case so a stalled or disconnected device can
/// never hang the caller.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// How long to sleep between checks of how many frames have been written,
/// while waiting for the tone (plus padding) to finish playing.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Play a sine wave tone at `hertz` for `millis` milliseconds on the
/// default output device, blocking until playback has actually finished
/// (or `DRAIN_TIMEOUT` has elapsed). A duration of 0 plays nothing (and
/// does not touch the audio device at all). Missing audio devices,
/// unsupported formats and stream errors are returned as errors, never
/// panics.
pub fn beep_with_hz_and_millis(hertz: u32, millis: u32) -> Result<(), String> {
    if millis == 0 {
        return Ok(());
    }
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| "no audio output device available".to_string())?;
    let config = device
        .default_output_config()
        .map_err(|e| format!("audio output config: {e}"))?;

    match config.sample_format() {
        cpal::SampleFormat::I8 => run::<i8>(&device, &config, hertz, millis),
        cpal::SampleFormat::I16 => run::<i16>(&device, &config, hertz, millis),
        cpal::SampleFormat::I32 => run::<i32>(&device, &config, hertz, millis),
        cpal::SampleFormat::I64 => run::<i64>(&device, &config, hertz, millis),
        cpal::SampleFormat::U8 => run::<u8>(&device, &config, hertz, millis),
        cpal::SampleFormat::U16 => run::<u16>(&device, &config, hertz, millis),
        cpal::SampleFormat::U32 => run::<u32>(&device, &config, hertz, millis),
        cpal::SampleFormat::U64 => run::<u64>(&device, &config, hertz, millis),
        cpal::SampleFormat::F32 => run::<f32>(&device, &config, hertz, millis),
        cpal::SampleFormat::F64 => run::<f64>(&device, &config, hertz, millis),
        sample_format => Err(format!("unsupported sample format: {sample_format}")),
    }
}

/// Number of whole frames needed to play `millis` milliseconds of audio
/// at `sample_rate`.
fn tone_frames(sample_rate: u32, millis: u32) -> u64 {
    (sample_rate as u64 * millis as u64) / 1000
}

/// Number of frames the fade-in and fade-out each last, derived from
/// [`FADE_MILLIS`].
fn fade_frames(sample_rate: u32) -> u64 {
    (sample_rate as u64 * FADE_MILLIS as u64) / 1000
}

/// Pick a `Fixed` buffer size, in frames, close to [`TARGET_BUFFER_MILLIS`]
/// but clamped to the device's supported range. Returns `None` when the
/// device does not report a range at all, in which case the caller should
/// fall back to `BufferSize::Default`.
fn fixed_buffer_frames(
    sample_rate: u32,
    target_millis: u32,
    supported: cpal::SupportedBufferSize,
) -> Option<cpal::FrameCount> {
    match supported {
        cpal::SupportedBufferSize::Range { min, max } => {
            let target = ((sample_rate as u64 * target_millis as u64) / 1000).max(1);
            let target = target.min(cpal::FrameCount::MAX as u64) as cpal::FrameCount;
            Some(target.clamp(min, max))
        }
        cpal::SupportedBufferSize::Unknown => None,
    }
}

/// How many extra frames to wait for, beyond the tone itself, before the
/// stream is dropped - enough for at least one full device buffer to be
/// flushed. Falls back to a conservative 100ms estimate when the actual
/// negotiated buffer size could not be queried.
fn drain_frames(buffer_frames: Option<cpal::FrameCount>, sample_rate: u32) -> u64 {
    let frames = buffer_frames.unwrap_or_else(|| (sample_rate / 10).max(1));
    (frames as u64) * 2
}

/// Fade multiplier (0.0..=1.0) for `frame_index` within a tone of
/// `tone_frames` total frames, ramping up over the first `fade_frames`
/// frames and back down over the last `fade_frames` frames. Frames at or
/// past `tone_frames`, or a zero-length tone, are silent.
fn envelope(frame_index: u64, tone_frames: u64, fade_frames: u64) -> f32 {
    if tone_frames == 0 || frame_index >= tone_frames {
        return 0.0;
    }
    let fade = fade_frames.clamp(1, (tone_frames / 2).max(1));
    let frames_remaining = tone_frames - frame_index;
    if frame_index < fade {
        frame_index as f32 / fade as f32
    } else if frames_remaining <= fade {
        frames_remaining as f32 / fade as f32
    } else {
        1.0
    }
}

/// Value of the sine wave (with fade envelope applied) for `frame_index`,
/// or silence once `frame_index` reaches `tone_frames`. The phase is
/// taken modulo `sample_rate` so it stays well within `f32` precision no
/// matter how long the stream keeps running after the tone has ended.
fn enveloped_sample(
    frame_index: u64,
    tone_frames: u64,
    fade_frames: u64,
    sample_rate: u32,
    frequency: f32,
) -> f32 {
    if frame_index >= tone_frames {
        return 0.0;
    }
    let sample_rate = sample_rate.max(1);
    let phase = (frame_index % sample_rate as u64) as f32;
    let angle = phase * frequency * 2.0 * std::f32::consts::PI / sample_rate as f32;
    angle.sin() * envelope(frame_index, tone_frames, fade_frames)
}

/// Build and play the actual output stream for one concrete sample type,
/// then block until the tone has actually been written out (plus a
/// draining margin), or until [`DRAIN_TIMEOUT`] past the requested
/// duration has elapsed, whichever comes first.
fn run<T>(
    device: &cpal::Device,
    supported_config: &cpal::SupportedStreamConfig,
    hertz: u32,
    millis: u32,
) -> Result<(), String>
where
    T: SizedSample + FromSample<f32>,
{
    let sample_rate = supported_config.sample_rate();
    let channels = supported_config.channels() as usize;
    let frequency = hertz as f32;
    let tone_len = tone_frames(sample_rate, millis);
    let fade_len = fade_frames(sample_rate);

    let frames_written = Arc::new(AtomicU64::new(0));
    let (error_tx, error_rx) = mpsc::channel::<cpal::Error>();

    let requested_buffer = fixed_buffer_frames(
        sample_rate,
        TARGET_BUFFER_MILLIS,
        *supported_config.buffer_size(),
    );

    let stream = match requested_buffer {
        Some(frames) => build_stream::<T>(
            device,
            channels,
            sample_rate,
            cpal::BufferSize::Fixed(frames),
            tone_len,
            fade_len,
            frequency,
            Arc::clone(&frames_written),
            error_tx.clone(),
        )
        .or_else(|_| {
            build_stream::<T>(
                device,
                channels,
                sample_rate,
                cpal::BufferSize::Default,
                tone_len,
                fade_len,
                frequency,
                Arc::clone(&frames_written),
                error_tx.clone(),
            )
        }),
        None => build_stream::<T>(
            device,
            channels,
            sample_rate,
            cpal::BufferSize::Default,
            tone_len,
            fade_len,
            frequency,
            Arc::clone(&frames_written),
            error_tx.clone(),
        ),
    }
    .map_err(|e| format!("audio stream: {e}"))?;

    stream.play().map_err(|e| format!("audio playback: {e}"))?;

    let target = tone_len + drain_frames(stream.buffer_size().ok(), sample_rate);
    let deadline = Instant::now() + Duration::from_millis(millis as u64) + DRAIN_TIMEOUT;
    while Instant::now() < deadline {
        if let Ok(err) = error_rx.try_recv() {
            return Err(format!("audio playback: {err}"));
        }
        if frames_written.load(Ordering::Relaxed) >= target {
            break;
        }
        std::thread::sleep(POLL_INTERVAL);
    }

    Ok(())
}

/// Build (but do not play) one output stream: `tone_frames` frames of
/// enveloped sine wave followed by silence, with the cumulative frame
/// count published to `frames_written` after every callback and any
/// stream error sent on `error_tx` instead of being discarded.
#[allow(clippy::too_many_arguments)]
fn build_stream<T>(
    device: &cpal::Device,
    channels: usize,
    sample_rate: u32,
    buffer_size: cpal::BufferSize,
    tone_frames: u64,
    fade_frames: u64,
    frequency: f32,
    frames_written: Arc<AtomicU64>,
    error_tx: mpsc::Sender<cpal::Error>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
{
    let config = cpal::StreamConfig {
        channels: channels as cpal::ChannelCount,
        sample_rate,
        buffer_size,
    };
    let mut next_frame = 0u64;
    device.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            next_frame = write_data(
                data,
                channels,
                next_frame,
                tone_frames,
                fade_frames,
                sample_rate,
                frequency,
            );
            frames_written.store(next_frame, Ordering::Relaxed);
        },
        move |err| {
            // The receiving end may already be gone (stream dropped after
            // the draining wait finished); a dropped send is not an error.
            let _ = error_tx.send(err);
        },
        None,
    )
}

/// Fill every channel of every output frame with the next sample value,
/// starting at `start_frame`. Frames before `tone_frames` get the
/// enveloped sine wave; frames at or past it get silence. Returns the new
/// cumulative frame count (`start_frame` plus the number of frames in
/// `output`).
fn write_data<T>(
    output: &mut [T],
    channels: usize,
    start_frame: u64,
    tone_frames: u64,
    fade_frames: u64,
    sample_rate: u32,
    frequency: f32,
) -> u64
where
    T: Sample + FromSample<f32>,
{
    let mut frame_index = start_frame;
    for frame in output.chunks_mut(channels) {
        let value: T = T::from_sample(enveloped_sample(
            frame_index,
            tone_frames,
            fade_frames,
            sample_rate,
            frequency,
        ));
        for sample in frame.iter_mut() {
            *sample = value;
        }
        frame_index += 1;
    }
    frame_index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_duration_is_silent_and_ok() {
        // Must not touch audio at all, so it works on headless CI.
        assert_eq!(beep_with_hz_and_millis(440, 0), Ok(()));
    }

    #[test]
    fn tone_frames_rounds_down_to_whole_frames() {
        assert_eq!(tone_frames(48_000, 250), 12_000);
        assert_eq!(tone_frames(44_100, 1), 44);
        assert_eq!(tone_frames(48_000, 0), 0);
    }

    #[test]
    fn fade_frames_is_fade_millis_worth_of_frames() {
        assert_eq!(fade_frames(48_000), 240);
        assert_eq!(fade_frames(44_100), 220);
    }

    #[test]
    fn fixed_buffer_frames_clamps_into_range() {
        let range = cpal::SupportedBufferSize::Range {
            min: 256,
            max: 4096,
        };
        // 20ms at 48kHz is 960 frames, inside the range: used as-is.
        assert_eq!(fixed_buffer_frames(48_000, 20, range), Some(960));
        // A device that only supports large buffers: clamp up to the min.
        let narrow_high = cpal::SupportedBufferSize::Range {
            min: 8192,
            max: 16384,
        };
        assert_eq!(fixed_buffer_frames(48_000, 20, narrow_high), Some(8192));
        // A device that only supports tiny buffers: clamp down to the max.
        let narrow_low = cpal::SupportedBufferSize::Range { min: 16, max: 32 };
        assert_eq!(fixed_buffer_frames(48_000, 20, narrow_low), Some(32));
    }

    #[test]
    fn fixed_buffer_frames_is_none_when_unknown() {
        assert_eq!(
            fixed_buffer_frames(48_000, 20, cpal::SupportedBufferSize::Unknown),
            None
        );
    }

    #[test]
    fn drain_frames_doubles_the_actual_buffer_size() {
        assert_eq!(drain_frames(Some(512), 48_000), 1024);
    }

    #[test]
    fn drain_frames_falls_back_when_buffer_size_unknown() {
        // Falls back to ~100ms of frames, doubled.
        assert_eq!(drain_frames(None, 48_000), 9_600);
    }

    #[test]
    fn envelope_is_silent_outside_the_tone() {
        assert_eq!(envelope(0, 0, 10), 0.0);
        assert_eq!(envelope(100, 100, 10), 0.0);
        assert_eq!(envelope(150, 100, 10), 0.0);
    }

    #[test]
    fn envelope_ramps_up_then_holds_then_ramps_down() {
        let tone = 100;
        let fade = 10;
        assert_eq!(envelope(0, tone, fade), 0.0);
        assert_eq!(envelope(5, tone, fade), 0.5);
        assert_eq!(envelope(10, tone, fade), 1.0);
        assert_eq!(envelope(50, tone, fade), 1.0);
        assert_eq!(envelope(94, tone, fade), 0.6);
        assert_eq!(envelope(99, tone, fade), 0.1);
    }

    #[test]
    fn envelope_never_panics_on_very_short_tones() {
        for tone in 0..5u64 {
            for frame in 0..tone + 1 {
                let _ = envelope(frame, tone, 10);
            }
        }
    }

    #[test]
    fn enveloped_sample_is_silent_once_tone_ends() {
        assert_eq!(enveloped_sample(1_000, 500, 10, 48_000, 440.0), 0.0);
    }

    #[test]
    fn write_data_fills_all_channels_and_cuts_off_at_tone_end() {
        // 2 channels, 6 frames; tone lasts only the first 2 frames, no
        // fade so the amplitude is easy to reason about.
        let mut out = [0.0f32; 12];
        let next = write_data(&mut out, 2, 0, 2, 0, 8, 1.0);
        assert_eq!(next, 6);
        // Every frame has both channels equal to each other.
        for frame in out.chunks(2) {
            assert_eq!(frame[0], frame[1]);
        }
        // Everything from frame 2 onward (samples 4..12) is exact silence.
        assert_eq!(&out[4..12], &[0.0; 8]);
    }

    #[test]
    fn write_data_resumes_from_start_frame() {
        let mut out = [0.0f32; 2];
        let next = write_data(&mut out, 1, 5, 10, 0, 48_000, 440.0);
        assert_eq!(next, 7);
    }
}
