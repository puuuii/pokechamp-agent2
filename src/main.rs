mod audio;
mod hardware;
mod inference;
mod video;

use anyhow::Result;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, RwLock};
use std::thread;

use audio::CpalAudioPassthrough;
use hardware::{AudioPipeline, HardwareProfile};
use inference::{InferenceConfig, InferenceWorker, ModelInputResolution, PhaseRules, PhaseStatus};
use video::{CaptureService, CropArea, DisplayWindow, VideoConfig};

fn main() -> Result<()> {
    let video_config = VideoConfig::default();

    const ML_SUBSAMPLING_INTERVAL_FRAMES: u32 = 30;

    let capture_service = CaptureService::new(video_config, ML_SUBSAMPLING_INTERVAL_FRAMES);
    let (rx_display, rx_ml) = capture_service.spawn_loop()?;

    let crop_area = Arc::new(RwLock::new(CropArea::default_relative()));
    let phase_status: PhaseStatus = Arc::new(RwLock::new(String::new()));
    // 表示側の▶ボタンで立てる、手動フェーズ進行リクエスト。
    let manual_phase_advance = Arc::new(AtomicBool::new(false));

    thread::spawn(move || {
        let audio_pipeline =
            CpalAudioPassthrough::for_hardware(&HardwareProfile::AVERMEDIA_LIVE_GAMER_MINI_GC311);
        if let Err(e) = audio_pipeline.start() {
            eprintln!("Audio pipeline error: {e}");
        }
    });

    InferenceWorker::spawn(
        rx_ml,
        InferenceConfig {
            resolution: ModelInputResolution::STANDARD_1280X720,
        },
        PhaseRules::default(),
        Arc::clone(&crop_area),
        Arc::clone(&phase_status),
        Arc::clone(&manual_phase_advance),
    );

    let display_resolution = video_config.resolution();
    let mut window =
        DisplayWindow::open_uncapped("Switch Capture", display_resolution, &manual_phase_advance)?;

    println!("\n=================== クロップ調整操作 ===================");
    println!("  [矢印キー]           : 赤枠の移動 (X, Y)");
    println!("  [Shift + 矢印キー]   : 赤枠のサイズ変更 (Width, Height)");
    println!("========================================================\n");

    while window.is_open() {
        window.render_latest(&rx_display, &crop_area, &phase_status)?;
    }

    Ok(())
}
