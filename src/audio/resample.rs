use std::time::Duration;

const TARGET_AUDIO_LATENCY: Duration = Duration::from_millis(50);
const MILLISECONDS_PER_SECOND: u64 = 1000;

pub fn calculate_ring_buffer_capacity(sample_rate: u32, channel_count: u32) -> usize {
    let samples_per_second_all_channels = (sample_rate * channel_count) as u64;
    let latency_ms = TARGET_AUDIO_LATENCY.as_millis() as u64;

    ((samples_per_second_all_channels * latency_ms) / MILLISECONDS_PER_SECOND) as usize
}

#[inline(always)]
pub fn linear_interpolate(start_sample: f32, end_sample: f32, interpolation_factor: f32) -> f32 {
    start_sample + (end_sample - start_sample) * interpolation_factor
}
