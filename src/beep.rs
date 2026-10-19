//! Minimal cross-platform beep tone generator, built directly on `cpal`.
//!
//! This exists instead of depending on the `actually_beep` crate because
//! that crate pins `cpal` to `^0.15.3` - a version with no BSD/FreeBSD
//! backend at all (BSD support was only added to `cpal` in a later
//! release). Calling `cpal` directly lets us use a current version that
//! does support FreeBSD, which is a hard-required target for this project.
//!
//! Every beep starts with [`LEAD_IN_MILLIS`] of silence. Desktop sound
//! servers (e.g. WirePlumber for PipeWire) suspend an output after a few
//! seconds of idleness, and clipGPT always beeps after such a pause (it
//! waits for the LLM first). Waking a suspended device - USB audio in
//! particular - swallows the first couple of hundred milliseconds of
//! sound, so without the lead-in a 250 ms beep came out as a click or
//! nothing at all. Measured on a USB sink behind pipewire-alsa: no lead-in
//! gave a click, 150 ms and more gave the full tone.
//!
//! The stream is then kept open until the backend's own playback clock
//! says the last tone frame has been played (plus a small margin), instead
//! of just sleeping for the tone duration and dropping the stream - a drop
//! halts output immediately, discarding anything still buffered.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SizedSample};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

/// Silence played before the tone so a suspended output device has time
/// to wake up before the audible part starts.
const LEAD_IN_MILLIS: u32 = 250;

/// Fade-in/fade-out duration, in milliseconds, applied at the start and
/// end of the tone to avoid audible clicks.
const FADE_MILLIS: u32 = 5;

/// How long the stream stays open after the backend reports the last tone
/// frame as played, to absorb any latency it does not account for.
const PLAYBACK_MARGIN: Duration = Duration::from_millis(50);

/// Extra time, beyond lead-in plus tone, after which the stream is dropped
/// regardless of what the playback clock says. Bounds the worst case so a
/// stalled device or a backend with broken timestamps can never hang the
/// caller.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// How long to sleep between checks of the playback clock.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Sentinel stored in the shared end-of-tone timestamp until the data
/// callback has computed the real value.
const NOT_YET: u64 = u64::MAX;

/// Play a sine wave tone at `hertz` for `millis` milliseconds on the
/// default output device, preceded by [`LEAD_IN_MILLIS`] of silence, and
/// block until the backend reports it as played (or [`DRAIN_TIMEOUT`] past
/// the expected end has elapsed). A duration of 0 plays nothing (and does
/// not touch the audio device at all). Missing audio devices, unsupported
/// formats and stream errors are returned as errors, never panics.
pub fn beep_with_hz_and_millis(hertz: u32, millis: u32) -> Result<(), String> {
    if millis == 0 {
        return Ok(());
    }
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| "no audio output device available".to_string())?;
    let supported = device
        .default_output_config()
        .map_err(|e| format!("audio output config: {e}"))?;
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();

    match format {
        cpal::SampleFormat::I8 => run::<i8>(&device, config, hertz, millis),
        cpal::SampleFormat::I16 => run::<i16>(&device, config, hertz, millis),
        cpal::SampleFormat::I32 => run::<i32>(&device, config, hertz, millis),
        cpal::SampleFormat::I64 => run::<i64>(&device, config, hertz, millis),
        cpal::SampleFormat::U8 => run::<u8>(&device, config, hertz, millis),
        cpal::SampleFormat::U16 => run::<u16>(&device, config, hertz, millis),
        cpal::SampleFormat::U32 => run::<u32>(&device, config, hertz, millis),
        cpal::SampleFormat::U64 => run::<u64>(&device, config, hertz, millis),
        cpal::SampleFormat::F32 => run::<f32>(&device, config, hertz, millis),
        cpal::SampleFormat::F64 => run::<f64>(&device, config, hertz, millis),
        sample_format => Err(format!("unsupported sample format: {sample_format}")),
    }
}

/// Number of whole frames that `millis` milliseconds take at
/// `sample_rate`.
fn frames_for_millis(sample_rate: u32, millis: u32) -> u64 {
    (sample_rate as u64 * millis as u64) / 1000
}

/// Where the tone sits in the stream, in frames: silence up to `start`,
/// tone from `start` (inclusive) to `end` (exclusive), silence after.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ToneLayout {
    start: u64,
    end: u64,
    fade: u64,
}

impl ToneLayout {
    /// Lay out a tone of `millis` ms after [`LEAD_IN_MILLIS`] of silence,
    /// with [`FADE_MILLIS`] fades, at `sample_rate`.
    fn new(sample_rate: u32, millis: u32) -> Self {
        let start = frames_for_millis(sample_rate, LEAD_IN_MILLIS);
        ToneLayout {
            start,
            end: start + frames_for_millis(sample_rate, millis),
            fade: frames_for_millis(sample_rate, FADE_MILLIS),
        }
    }
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

/// Sample value for stream frame `frame_index`: silence outside the tone,
/// otherwise the faded sine wave. The phase is counted from the start of
/// the tone and taken modulo `sample_rate`, so it stays well within `f32`
/// precision.
fn sample_at(frame_index: u64, layout: ToneLayout, sample_rate: u32, frequency: f32) -> f32 {
    if frame_index < layout.start || frame_index >= layout.end {
        return 0.0;
    }
    let sample_rate = sample_rate.max(1);
    let k = frame_index - layout.start;
    let phase = (k % sample_rate as u64) as f32;
    let angle = phase * frequency * 2.0 * std::f32::consts::PI / sample_rate as f32;
    angle.sin() * envelope(k, layout.end - layout.start, layout.fade)
}

/// If frame `end_frame` falls inside the callback buffer that starts at
/// frame `first_frame` and holds `frames` frames, return the stream-clock
/// time (in nanoseconds) at which it will be played, given that the
/// buffer's first frame plays at `playback_nanos`. Otherwise `None`.
fn end_playback_nanos(
    first_frame: u64,
    frames: u64,
    end_frame: u64,
    playback_nanos: u64,
    sample_rate: u32,
) -> Option<u64> {
    if end_frame < first_frame || end_frame >= first_frame + frames {
        return None;
    }
    let offset = (end_frame - first_frame) * 1_000_000_000 / sample_rate.max(1) as u64;
    Some(playback_nanos.saturating_add(offset))
}

/// Whether the tone has been played, i.e. the stream clock `now_nanos`
/// has passed the end-of-tone time `end_nanos` (or [`NOT_YET`]) by at
/// least `margin`.
fn playback_finished(now_nanos: u64, end_nanos: u64, margin: Duration) -> bool {
    end_nanos != NOT_YET && now_nanos >= end_nanos.saturating_add(margin.as_nanos() as u64)
}

/// Build and play the output stream for one concrete sample type, then
/// block until the backend's playback clock has passed the end of the
/// tone by [`PLAYBACK_MARGIN`], or until [`DRAIN_TIMEOUT`] past the
/// expected end has elapsed, whichever comes first. Stream errors are
/// returned instead of being discarded.
fn run<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    hertz: u32,
    millis: u32,
) -> Result<(), String>
where
    T: SizedSample + FromSample<f32>,
{
    let sample_rate = config.sample_rate;
    let channels = (config.channels as usize).max(1);
    let frequency = hertz as f32;
    let layout = ToneLayout::new(sample_rate, millis);

    let end_nanos = Arc::new(AtomicU64::new(NOT_YET));
    let end_nanos_cb = Arc::clone(&end_nanos);
    let (error_tx, error_rx) = mpsc::channel::<cpal::Error>();
    let mut next_frame = 0u64;

    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
                let first = next_frame;
                next_frame = write_data(data, channels, first, layout, sample_rate, frequency);
                let playback = info.timestamp().playback.as_nanos();
                let playback = u64::try_from(playback).unwrap_or(NOT_YET - 1);
                if let Some(t) =
                    end_playback_nanos(first, next_frame - first, layout.end, playback, sample_rate)
                {
                    end_nanos_cb.store(t, Ordering::Relaxed);
                }
            },
            move |err| {
                // The receiver is gone once the stream is being dropped;
                // a failed send then is not an error.
                let _ = error_tx.send(err);
            },
            None,
        )
        .map_err(|e| format!("audio stream: {e}"))?;

    stream.play().map_err(|e| format!("audio playback: {e}"))?;

    let deadline = Instant::now()
        + Duration::from_millis(LEAD_IN_MILLIS as u64 + millis as u64)
        + DRAIN_TIMEOUT;
    while Instant::now() < deadline {
        if let Ok(err) = error_rx.try_recv() {
            return Err(format!("audio playback: {err}"));
        }
        let now = u64::try_from(stream.now().as_nanos()).unwrap_or(NOT_YET - 1);
        if playback_finished(now, end_nanos.load(Ordering::Relaxed), PLAYBACK_MARGIN) {
            break;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    Ok(())
}

/// Fill every channel of every output frame with the sample for its
/// stream position, starting at frame `first_frame`. Returns the position
/// of the frame after the last one written.
fn write_data<T>(
    output: &mut [T],
    channels: usize,
    first_frame: u64,
    layout: ToneLayout,
    sample_rate: u32,
    frequency: f32,
) -> u64
where
    T: Sample + FromSample<f32>,
{
    let mut frame_index = first_frame;
    for frame in output.chunks_mut(channels) {
        let value: T = T::from_sample(sample_at(frame_index, layout, sample_rate, frequency));
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
    fn frames_for_millis_rounds_down() {
        assert_eq!(frames_for_millis(48_000, 250), 12_000);
        assert_eq!(frames_for_millis(44_100, 1), 44);
        assert_eq!(frames_for_millis(48_000, 0), 0);
    }

    #[test]
    fn layout_puts_tone_after_lead_in() {
        let l = ToneLayout::new(48_000, 250);
        assert_eq!(l.start, 48 * LEAD_IN_MILLIS as u64);
        assert_eq!(l.end, l.start + 12_000);
        assert_eq!(l.fade, 48 * FADE_MILLIS as u64);
    }

    #[test]
    fn lead_in_is_long_enough_to_wake_a_suspended_device() {
        // 150 ms was the shortest lead-in measured to give a full beep on
        // a suspended USB sink; keep a margin above it.
        const { assert!(LEAD_IN_MILLIS >= 200) };
    }

    #[test]
    fn envelope_is_silent_outside_the_tone() {
        assert_eq!(envelope(0, 0, 10), 0.0);
        assert_eq!(envelope(100, 100, 10), 0.0);
        assert_eq!(envelope(150, 100, 10), 0.0);
    }

    #[test]
    fn envelope_ramps_up_then_holds_then_ramps_down() {
        assert_eq!(envelope(0, 100, 10), 0.0);
        assert_eq!(envelope(5, 100, 10), 0.5);
        assert_eq!(envelope(10, 100, 10), 1.0);
        assert_eq!(envelope(50, 100, 10), 1.0);
        assert_eq!(envelope(94, 100, 10), 0.6);
        assert_eq!(envelope(99, 100, 10), 0.1);
    }

    #[test]
    fn envelope_never_panics_on_very_short_tones() {
        for tone in 0..5u64 {
            for frame in 0..=tone {
                let _ = envelope(frame, tone, 10);
            }
        }
    }

    #[test]
    fn samples_are_silent_before_and_after_the_tone() {
        let l = ToneLayout {
            start: 100,
            end: 200,
            fade: 0,
        };
        for i in (0..100).chain(200..300) {
            assert_eq!(sample_at(i, l, 48_000, 1046.0), 0.0, "frame {i}");
        }
        assert!((100..200).any(|i| sample_at(i, l, 48_000, 1046.0) != 0.0));
    }

    #[test]
    fn tone_phase_starts_at_zero_after_lead_in() {
        let l = ToneLayout {
            start: 100,
            end: 200,
            fade: 0,
        };
        // sin(0) at the first tone frame, so the tone starts without a jump.
        assert_eq!(sample_at(100, l, 48_000, 1046.0), 0.0);
    }

    #[test]
    fn end_playback_found_only_in_the_right_buffer() {
        // 48 kHz: one frame is 20833 ns.
        assert_eq!(end_playback_nanos(0, 512, 600, 1_000, 48_000), None);
        assert_eq!(end_playback_nanos(1024, 512, 600, 1_000, 48_000), None);
        assert_eq!(
            end_playback_nanos(512, 512, 512, 1_000, 48_000),
            Some(1_000)
        );
        assert_eq!(
            end_playback_nanos(512, 512, 560, 1_000, 48_000),
            Some(1_000 + 48 * 1_000_000_000 / 48_000)
        );
        // The frame just past the buffer belongs to the next one.
        assert_eq!(end_playback_nanos(512, 512, 1024, 1_000, 48_000), None);
    }

    #[test]
    fn playback_finished_waits_for_margin() {
        let m = Duration::from_millis(50);
        assert!(!playback_finished(10, NOT_YET, m));
        assert!(!playback_finished(1_000, 1_000, m));
        assert!(!playback_finished(1_000 + 49_999_999, 1_000, m));
        assert!(playback_finished(1_000 + 50_000_000, 1_000, m));
    }

    #[test]
    fn write_data_fills_all_channels_and_returns_next_frame() {
        let l = ToneLayout {
            start: 2,
            end: 4,
            fade: 0,
        };
        let mut out = [1.0f32; 12];
        let next = write_data(&mut out, 2, 0, l, 8, 1.0);
        assert_eq!(next, 6);
        for frame in out.chunks(2) {
            assert_eq!(frame[0], frame[1]);
        }
        // Lead-in frames 0..2 and frames 4..6 after the tone are silence.
        assert_eq!(&out[0..4], &[0.0; 4]);
        assert_eq!(&out[8..12], &[0.0; 4]);
        // Tone frame 3 (phase 1/8 of a 1 Hz wave at 8 Hz) is audible.
        assert!(out[6] > 0.0);
    }

    #[test]
    fn write_data_resumes_from_first_frame() {
        let l = ToneLayout::new(48_000, 250);
        let mut out = [0.0f32; 2];
        assert_eq!(write_data(&mut out, 1, 5, l, 48_000, 440.0), 7);
    }
}
