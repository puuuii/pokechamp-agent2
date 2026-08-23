mod resample;

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::{HeapCons, HeapProd, HeapRb, traits::{Consumer, Split}};
use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tracing::{error, info};

use resample::{calculate_ring_buffer_capacity, LinearResampler, SILENCE_SAMPLE};

use crate::hardware::{AudioPipeline, HardwareProfile};

/// シャットダウン信号のポーリング間隔。
const SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// 音声パススルーのパラメータ。
/// 通常は TOML ファイル(config/audio.toml)から読み込む。
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct AudioConfig {
    /// 目標の音声遅延(ミリ秒)。
    /// リングバッファの容量はこの値から導出される。
    pub target_latency_millis: u64,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            target_latency_millis: 50,
        }
    }
}

pub struct CpalAudioPassthrough {
    target_device_keyword: String,
    device_name: &'static str,
    target_latency: Duration,
    shutdown: Arc<AtomicBool>,
}

impl CpalAudioPassthrough {
    pub fn for_hardware(
        profile: &HardwareProfile,
        config: AudioConfig,
        shutdown: Arc<AtomicBool>,
    ) -> Self {
        Self {
            target_device_keyword: profile.audio_device_keyword.to_lowercase(),
            device_name: profile.name,
            target_latency: Duration::from_millis(config.target_latency_millis),
            shutdown,
        }
    }

    /// デバイス名から対象キャプチャデバイスを探し出す。
    fn find_input_device(host: &cpal::Host, keyword: &str) -> Result<cpal::Device> {
        let device = host
            .input_devices()?
            .find(|device| {
                device
                    .name()
                    .map(|name| name.to_lowercase().contains(keyword))
                    .unwrap_or(false)
            })
            .context("Target capture audio device not found")?;
        Ok(device)
    }

    /// 入力ストリームを構築する。リサンプリングしてリングバッファへpushする。
    fn build_input_stream(
        device: cpal::Device,
        config: cpal::SupportedStreamConfig,
        mut audio_producer: HeapProd<f32>,
        mut resampler: LinearResampler,
    ) -> Result<cpal::Stream> {
        let stream = device.build_input_stream(
            &config.into(),
            move |raw_input_data: &[f32], _| {
                resampler.resample_into(raw_input_data, &mut audio_producer);
                resampler.report_dropped_samples_if_due();
            },
            move |err| error!("Audio input stream error: {err}"),
            None,
        )?;
        Ok(stream)
    }

    /// 出力ストリームを構築する。リングバッファからpopして書き出す。
    fn build_output_stream(
        device: cpal::Device,
        config: cpal::SupportedStreamConfig,
        mut audio_consumer: HeapCons<f32>,
    ) -> Result<cpal::Stream> {
        let stream = device.build_output_stream(
            &config.into(),
            move |output_buffer: &mut [f32], _| {
                for destination_sample in output_buffer.iter_mut() {
                    *destination_sample = audio_consumer.try_pop().unwrap_or(SILENCE_SAMPLE);
                }
            },
            move |err| error!("Audio output stream error: {err}"),
            None,
        )?;
        Ok(stream)
    }
}

impl AudioPipeline for CpalAudioPassthrough {
    fn start(self) -> Result<()> {
        info!(
            device = %self.device_name,
            target_latency = ?self.target_latency,
            "Starting audio pipeline"
        );
        let host = cpal::default_host();

        let input_device = Self::find_input_device(&host, &self.target_device_keyword)?;
        let output_device = host
            .default_output_device()
            .context("Default output device not found")?;

        let input_config = input_device.default_input_config()?;
        let output_config = output_device.default_output_config()?;

        let in_channels = input_config.channels() as usize;
        let in_sample_rate = input_config.sample_rate().0;
        let out_channels = output_config.channels() as usize;
        let out_sample_rate = output_config.sample_rate().0;

        info!("Audio Input: {in_channels} ch, {in_sample_rate} Hz");
        info!("Audio Output: {out_channels} ch, {out_sample_rate} Hz");

        let buffer_capacity =
            calculate_ring_buffer_capacity(out_sample_rate, out_channels as u32, self.target_latency);
        let ring_buffer = HeapRb::<f32>::new(buffer_capacity);
        let (audio_producer, audio_consumer) = ring_buffer.split();

        let sample_rate_resample_ratio = out_sample_rate as f64 / in_sample_rate as f64;
        let resampler =
            LinearResampler::new(in_channels, out_channels, sample_rate_resample_ratio);

        let input_stream =
            Self::build_input_stream(input_device, input_config, audio_producer, resampler)?;
        let output_stream =
            Self::build_output_stream(output_device, output_config, audio_consumer)?;

        input_stream.play()?;
        output_stream.play()?;

        // シャットダウン信号を待つ。
        // ストリームは関数返却時にローカルがdropされ停止する。
        while !self.shutdown.load(Ordering::Relaxed) {
            thread::sleep(SHUTDOWN_POLL_INTERVAL);
        }
        drop(input_stream);
        drop(output_stream);
        Ok(())
    }
}