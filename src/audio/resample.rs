use ringbuf::{HeapProd, traits::Producer};
use std::time::{Duration, Instant};
use tracing::warn;

const MILLISECONDS_PER_SECOND: u64 = 1000;

/// ドロップ警告ログの間隔。
const DROPPED_SAMPLE_WARN_INTERVAL: Duration = Duration::from_secs(1);

pub const SILENCE_SAMPLE: f32 = 0.0;

/// 目標の音声遅延からリングバッファの容量を算出する。
pub fn calculate_ring_buffer_capacity(
    sample_rate: u32,
    channel_count: u32,
    target_latency: Duration,
) -> usize {
    let samples_per_second_all_channels = (sample_rate * channel_count) as u64;
    let latency_ms = target_latency.as_millis() as u64;

    ((samples_per_second_all_channels * latency_ms) / MILLISECONDS_PER_SECOND) as usize
}

#[inline(always)]
pub fn linear_interpolate(start_sample: f32, end_sample: f32, interpolation_factor: f32) -> f32 {
    start_sample + (end_sample - start_sample) * interpolation_factor
}

/// 入力ブロックのidx番フレームのsliceを返す(0..frame_count-1にクランプ)。
/// `frame_count` は 1 以上であること。
#[inline(always)]
fn frame_at(input: &[f32], idx: usize, frame_count: usize, channels: usize) -> &[f32] {
    let idx = idx.min(frame_count - 1);
    let start = idx * channels;
    &input[start..start + channels]
}

/// 入力ストリームブロックをまたぐ線形リサンプリア。
///
/// 補間位置が現在のブロック先頭より前に落ちた場合に前ブロックの末尾フレームを
/// 借りる `previous_block_last_frame` の状態を持つ。これにより、
/// input_stream コールバックの外側(単体テストなど)でもリサンプリングを走らせる。
pub struct LinearResampler {
    ratio: f64,
    in_channels: usize,
    out_channels: usize,
    previous_block_last_frame: Vec<f32>,
    /// リンクバッファ溢れで破棄したサンプル数。
    dropped_samples: u64,
    /// 直前のドロップ警告ログの発行時刻。
    last_drop_report: Instant,
}

impl LinearResampler {
    /// ratioは出力サンプルレート/入力サンプルレート。
    pub fn new(in_channels: usize, out_channels: usize, ratio: f64) -> Self {
        Self {
            ratio,
            in_channels,
            out_channels,
            previous_block_last_frame: vec![SILENCE_SAMPLE; in_channels],
            dropped_samples: 0,
            last_drop_report: Instant::now(),
        }
    }

    /// `input`を`producer`へリサンプリングして書き込む。
    ///
    /// `input`の長さは`in_channels`の整数倍でなければならない。
    pub fn resample_into(&mut self, input: &[f32], producer: &mut HeapProd<f32>) {
        let input_frame_count = input.len() / self.in_channels;
        if input_frame_count == 0 {
            return;
        }

        let output_frame_count =
            ((input_frame_count as f64) * self.ratio).round() as usize;

        for output_index in 0..output_frame_count {
            let source_position = output_index as f64 / self.ratio;
            let lower_frame_index = source_position.floor() as isize;
            let interpolation_fraction = (source_position - source_position.floor()) as f32;

            let start_frame: &[f32] = if lower_frame_index < 0 {
                &self.previous_block_last_frame
            } else {
                frame_at(input, lower_frame_index as usize, input_frame_count, self.in_channels)
            };

            let end_frame: &[f32] =
                frame_at(input, (lower_frame_index + 1) as usize, input_frame_count, self.in_channels);

            for out_ch in 0..self.out_channels {
                let in_ch = if out_ch < self.in_channels { out_ch } else { 0 };

                let resampled_value = linear_interpolate(
                    start_frame[in_ch],
                    end_frame[in_ch],
                    interpolation_fraction,
                );

                if producer.try_push(resampled_value).is_err() {
                    self.dropped_samples += 1;
                }
            }
        }

        let last_frame_start = (input_frame_count - 1) * self.in_channels;
        self.previous_block_last_frame.copy_from_slice(
            &input[last_frame_start..last_frame_start + self.in_channels],
        );
    }

    /// 破棄が起きている場合、1秒に1回まで警告ログを出力する。
    ///
    /// 報告後はカウンタをリセットするため、ログは直前報告以降の破棄数を表す。
    pub fn report_dropped_samples_if_due(&mut self) {
        if self.dropped_samples == 0
            || self.last_drop_report.elapsed() < DROPPED_SAMPLE_WARN_INTERVAL
        {
            return;
        }
        let dropped = self.dropped_samples;
        self.dropped_samples = 0;
        self.last_drop_report = Instant::now();
        warn!("リンクバッファが埋まって {dropped} サンプルを破棄しました");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ringbuf::{HeapRb, traits::{Consumer, Split}};

    fn run(input: &[f32], in_channels: usize, out_channels: usize, ratio: f64) -> Vec<f32> {
        let mut resampler = LinearResampler::new(in_channels, out_channels, ratio);
        let ring_buffer = HeapRb::<f32>::new(256);
        let (mut producer, mut consumer) = ring_buffer.split();
        resampler.resample_into(input, &mut producer);

        let mut samples = Vec::new();
        while let Some(sample) = consumer.try_pop() {
            samples.push(sample);
        }
        samples
    }

    #[test]
    fn upsamples_with_linear_interpolation() {
        let samples = run(&[0.0, 0.5, 1.0], 1, 1, 2.0);
        assert_eq!(samples, vec![0.0, 0.25, 0.5, 0.75, 1.0, 1.0]);
    }

    #[test]
    fn downsamples_picking_source_frames() {
        let samples = run(&[0.0, 1.0, 2.0, 3.0], 1, 1, 0.5);
        assert_eq!(samples, vec![0.0, 2.0]);
    }

    #[test]
    fn upsamples_each_channel_independently() {
        let samples = run(&[1.0, 10.0, 3.0, 20.0], 2, 2, 1.0);
        assert_eq!(samples, vec![1.0, 10.0, 3.0, 20.0]);
    }

    #[test]
    fn downmixes_output_channels_onto_input_channel_zero() {
        let samples = run(&[1.0, 10.0, 2.0, 20.0], 2, 1, 1.0);
        assert_eq!(samples, vec![1.0, 2.0]);
    }

    #[test]
    fn consecutive_blocks_keep_per_block_output_count() {
        let mut resampler = LinearResampler::new(1, 1, 2.0);
        let ring_buffer = HeapRb::<f32>::new(64);
        let (mut producer, mut consumer) = ring_buffer.split();

        resampler.resample_into(&[1.0, 2.0], &mut producer);
        resampler.resample_into(&[3.0, 4.0], &mut producer);

        let mut samples = Vec::new();
        while let Some(sample) = consumer.try_pop() {
            samples.push(sample);
        }
        assert_eq!(samples.len(), 8);
    }

    #[test]
    fn empty_input_produces_nothing() {
        assert!(run(&[], 1, 1, 2.0).is_empty());
    }

    #[test]
    fn frame_at_clamps_index_to_bounds() {
        let input = [0.0, 1.0, 2.0, 3.0];
        assert_eq!(frame_at(&input, 0, 2, 2)[0], 0.0);
        assert_eq!(frame_at(&input, 0, 2, 2)[1], 1.0);
        assert_eq!(frame_at(&input, 1, 2, 2)[0], 2.0);
        assert_eq!(frame_at(&input, 1, 2, 2)[1], 3.0);
        // 範囲外のindexは最終フレームにクランプされる。
        assert_eq!(frame_at(&input, 99, 2, 2)[0], 2.0);
        assert_eq!(frame_at(&input, 99, 2, 2)[1], 3.0);
    }
}
