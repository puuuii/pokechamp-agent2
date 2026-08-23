mod resample;

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::{HeapRb, traits::{Consumer, Split}};
use std::thread;

use resample::{calculate_ring_buffer_capacity, LinearResampler, SILENCE_SAMPLE};

use crate::hardware::{AudioPipeline, HardwareProfile};

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
        let mut resampler =
            LinearResampler::new(in_channels, out_channels, sample_rate_resample_ratio);

        let input_stream = input_device.build_input_stream(
            &input_config.into(),
            move |raw_input_data: &[f32], _| {
                resampler.resample_into(raw_input_data, &mut audio_producer);
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

