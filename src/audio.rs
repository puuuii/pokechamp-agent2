use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::{
    HeapRb,
    traits::{Consumer, Producer, Split},
};
use std::thread;
use std::time::Duration;

use crate::hardware::{AudioPipeline, HardwareProfile};

const TARGET_AUDIO_LATENCY: Duration = Duration::from_millis(50);
const MILLISECONDS_PER_SECOND: u64 = 1000;
const SILENCE_SAMPLE: f32 = 0.0;

fn calculate_ring_buffer_capacity(sample_rate: u32, channel_count: u32) -> usize {
    let samples_per_second_all_channels = (sample_rate * channel_count) as u64;
    let latency_ms = TARGET_AUDIO_LATENCY.as_millis() as u64;

    ((samples_per_second_all_channels * latency_ms) / MILLISECONDS_PER_SECOND) as usize
}

#[inline(always)]
fn linear_interpolate(start_sample: f32, end_sample: f32, interpolation_factor: f32) -> f32 {
    start_sample + (end_sample - start_sample) * interpolation_factor
}

pub struct CpalAudioPassthrough {
    target_device_keyword: String,
}

impl CpalAudioPassthrough {
    pub fn for_hardware(profile: &HardwareProfile) -> Self {
        Self {
            target_device_keyword: profile.audio_device_keyword.to_lowercase(),
        }
    }
}

impl AudioPipeline for CpalAudioPassthrough {
    fn start(self) -> Result<()> {
        let host = cpal::default_host();

        let input_device = host
            .input_devices()?
            .find(|device| {
                device
                    .name()
                    .map(|name| name.to_lowercase().contains(&self.target_device_keyword))
                    .unwrap_or(false)
            })
            .context("Target capture audio device not found")?;

        let output_device = host
            .default_output_device()
            .context("Default output device not found")?;

        let input_config = input_device.default_input_config()?;
        let output_config = output_device.default_output_config()?;

        let in_channels = input_config.channels() as usize;
        let in_sample_rate = input_config.sample_rate().0;
        let out_channels = output_config.channels() as usize;
        let out_sample_rate = output_config.sample_rate().0;

        println!("Audio Input: {in_channels} ch, {in_sample_rate} Hz");
        println!("Audio Output: {out_channels} ch, {out_sample_rate} Hz");

        let buffer_capacity = calculate_ring_buffer_capacity(out_sample_rate, out_channels as u32);
        let ring_buffer = HeapRb::<f32>::new(buffer_capacity);
        let (mut audio_producer, mut audio_consumer) = ring_buffer.split();

        let sample_rate_resample_ratio = out_sample_rate as f64 / in_sample_rate as f64;

        let mut previous_block_last_frame = vec![SILENCE_SAMPLE; in_channels];

        let input_stream = input_device.build_input_stream(
            &input_config.into(),
            move |raw_input_data: &[f32], _| {
                if raw_input_data.is_empty() {
                    return;
                }

                let input_frame_count = raw_input_data.len() / in_channels;
                if input_frame_count == 0 {
                    return;
                }

                let output_frame_count =
                    ((input_frame_count as f64) * sample_rate_resample_ratio).round() as usize;

                for output_index in 0..output_frame_count {
                    let source_position = output_index as f64 / sample_rate_resample_ratio;
                    let lower_frame_index = source_position.floor() as isize;
                    let interpolation_fraction = (source_position - source_position.floor()) as f32;

                    let start_frame: &[f32] = if lower_frame_index < 0 {
                        &previous_block_last_frame
                    } else {
                        let start_idx = lower_frame_index as usize * in_channels;
                        &raw_input_data[start_idx..start_idx + in_channels]
                    };

                    let end_frame: &[f32] = if (lower_frame_index + 1) as usize >= input_frame_count
                    {
                        let last_idx = (input_frame_count - 1) * in_channels;
                        &raw_input_data[last_idx..last_idx + in_channels]
                    } else {
                        let end_idx = (lower_frame_index + 1) as usize * in_channels;
                        &raw_input_data[end_idx..end_idx + in_channels]
                    };

                    for out_ch in 0..out_channels {
                        let in_ch = if out_ch < in_channels { out_ch } else { 0 };

                        let resampled_value = linear_interpolate(
                            start_frame[in_ch],
                            end_frame[in_ch],
                            interpolation_fraction,
                        );

                        let _ = audio_producer.try_push(resampled_value);
                    }
                }

                let last_frame_start = (input_frame_count - 1) * in_channels;
                previous_block_last_frame.copy_from_slice(
                    &raw_input_data[last_frame_start..last_frame_start + in_channels],
                );
            },
            move |err| eprintln!("Audio input stream error: {err}"),
            None,
        )?;

        let output_stream = output_device.build_output_stream(
            &output_config.into(),
            move |output_buffer: &mut [f32], _| {
                for destination_sample in output_buffer.iter_mut() {
                    *destination_sample = audio_consumer.try_pop().unwrap_or(SILENCE_SAMPLE);
                }
            },
            move |err| eprintln!("Audio output stream error: {err}"),
            None,
        )?;

        input_stream.play()?;
        output_stream.play()?;

        thread::park();
        Ok(())
    }
}
