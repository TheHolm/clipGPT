//! Minimal cross-platform beep tone generator, built directly on `cpal`.
//!
//! This exists instead of depending on the `actually_beep` crate because
//! that crate pins `cpal` to `^0.15.3` - a version with no BSD/FreeBSD
//! backend at all (BSD support was only added to `cpal` in a later
//! release). Calling `cpal` directly lets us use a current version that
//! does support FreeBSD, which is a hard-required target for this project.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SizedSample};
use std::time::Duration;

/// Play a sine wave tone at `hertz` for `millis` milliseconds on the
/// default output device, blocking until playback finishes. A duration of
/// 0 plays nothing (and does not touch the audio device at all). Missing
/// audio devices and unsupported formats are returned as errors, never
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

    let result = match config.sample_format() {
        cpal::SampleFormat::I8 => run::<i8>(&device, &config.into(), hertz, millis),
        cpal::SampleFormat::I16 => run::<i16>(&device, &config.into(), hertz, millis),
        cpal::SampleFormat::I32 => run::<i32>(&device, &config.into(), hertz, millis),
        cpal::SampleFormat::I64 => run::<i64>(&device, &config.into(), hertz, millis),
        cpal::SampleFormat::U8 => run::<u8>(&device, &config.into(), hertz, millis),
        cpal::SampleFormat::U16 => run::<u16>(&device, &config.into(), hertz, millis),
        cpal::SampleFormat::U32 => run::<u32>(&device, &config.into(), hertz, millis),
        cpal::SampleFormat::U64 => run::<u64>(&device, &config.into(), hertz, millis),
        cpal::SampleFormat::F32 => run::<f32>(&device, &config.into(), hertz, millis),
        cpal::SampleFormat::F64 => run::<f64>(&device, &config.into(), hertz, millis),
        sample_format => return Err(format!("unsupported sample format: {sample_format}")),
    };
    result.map_err(|e| format!("audio playback: {e}"))
}

/// Build and play the actual output stream for one concrete sample type.
fn run<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    hertz: u32,
    millis: u32,
) -> Result<(), cpal::Error>
where
    T: SizedSample + FromSample<f32>,
{
    let sample_rate = config.sample_rate as f32;
    let channels = config.channels as usize;
    let frequency = hertz as f32;

    // Produce a sinusoid of maximum amplitude.
    let mut sample_clock = 0f32;
    let mut next_value = move || {
        sample_clock = (sample_clock + 1.0) % sample_rate;
        (sample_clock * frequency * 2.0 * std::f32::consts::PI / sample_rate).sin()
    };

    let stream = device.build_output_stream(
        *config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            write_data(data, channels, &mut next_value)
        },
        |_err| {},
        None,
    )?;
    stream.play()?;

    std::thread::sleep(Duration::from_millis(millis as u64));

    Ok(())
}

/// Fill every channel of every output frame with the next sample value.
fn write_data<T>(output: &mut [T], channels: usize, next_sample: &mut dyn FnMut() -> f32)
where
    T: Sample + FromSample<f32>,
{
    for frame in output.chunks_mut(channels) {
        let value: T = T::from_sample(next_sample());
        for sample in frame.iter_mut() {
            *sample = value;
        }
    }
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
    fn write_data_fills_all_channels() {
        let mut out = [0.0f32; 6];
        let mut n = 0.0;
        let mut next = || {
            n += 0.25;
            n
        };
        write_data(&mut out, 2, &mut next);
        assert_eq!(out, [0.25, 0.25, 0.5, 0.5, 0.75, 0.75]);
    }
}
