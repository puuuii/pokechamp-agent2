mod audio;
mod hardware;
mod inference;
mod video;

use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use tracing::error;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

use audio::CpalAudioPassthrough;
use hardware::{AudioPipeline, HardwareProfile};
use inference::{InferenceConfig, InferenceWorker, PhaseRules, PhaseStatus};
use video::{CaptureService, CropArea, DisplayPanelConfig, DisplayWindow, VideoConfig};

const PHASE_RULES_CONFIG_PATH: &str = "config/phase_rules.toml";
const INFERENCE_CONFIG_PATH: &str = "config/inference.toml";
const DISPLAY_CONFIG_PATH: &str = "config/display.toml";

/// ロギング初期化(RUST_LOG 環境変数、未設定時は info)。
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .init();
}

fn main() -> Result<()> {
    let video_config = VideoConfig::default();

    const ML_SUBSAMPLING_INTERVAL_FRAMES: u32 = 30;

    let capture_service = CaptureService::new(video_config, ML_SUBSAMPLING_INTERVAL_FRAMES);
    init_tracing();

    let phase_rules = match PhaseRules::load(PHASE_RULES_CONFIG_PATH) {
        Ok(rules) => rules,
        Err(e) => {
            error!("フェーズルールの設定を読み込めませんでした: {e}。組み込みデフォルトを使います。");
            PhaseRules::default()
        }
    };
    let inference_config = match InferenceConfig::load(INFERENCE_CONFIG_PATH) {
        Ok(config) => config,
        Err(e) => {
            error!("推論の設定を読み込めませんでした: {e}。組み込みデフォルトを使います。");
            InferenceConfig::default()
        }
    };
    let panel_config = match DisplayPanelConfig::load(DISPLAY_CONFIG_PATH) {
        Ok(config) => config,
        Err(e) => {
            error!("表示の設定を読み込めませんでした: {e}。組み込みデフォルトを使います。");
            DisplayPanelConfig::default()
        }
    };

    // 全体シャットダウンフラグ。表示ウィンドウクローズで立てる。
    let shutdown = Arc::new(AtomicBool::new(false));

    let (rx_display, rx_ml, capture_handle) =
        capture_service.spawn_loop(Arc::clone(&shutdown))?;

    let crop_area = Arc::new(RwLock::new(CropArea::default_relative()));
    let phase_status: PhaseStatus = Arc::new(RwLock::new(String::new()));
    // 表示側の▶ボタンで立てる、手動フェーズ進行リクエスト。
    let manual_phase_advance = Arc::new(AtomicBool::new(false));

    let shutdown_audio = Arc::clone(&shutdown);
    let audio_handle = thread::spawn(move || {
        let audio_pipeline = CpalAudioPassthrough::for_hardware(
            &HardwareProfile::AVERMEDIA_LIVE_GAMER_MINI_GC311,
            shutdown_audio,
        );
        if let Err(e) = audio_pipeline.start() {
            error!("Audio pipeline error: {e}");
        }
    });

    let inference_handle = InferenceWorker::spawn(
        rx_ml,
        inference_config,
        phase_rules,
        Arc::clone(&crop_area),
        Arc::clone(&phase_status),
        Arc::clone(&manual_phase_advance),
        Arc::clone(&shutdown),
    );

    let display_resolution = video_config.resolution();
    let mut window = DisplayWindow::open_uncapped(
        "Switch Capture",
        display_resolution,
        panel_config,
        &manual_phase_advance,
    )?;

    println!("\n=================== クロップ調整操作 ===================");
    println!("  [矢印キー]           : 赤枠の移動 (X, Y)");
    println!("  [Shift + 矢印キー]   : 赤枠のサイズ変更 (Width, Height)");
    println!("========================================================\n");

    while window.is_open() {
        if let Err(e) = window.render_latest(&rx_display, &crop_area, &phase_status) {
            error!("Display render error: {e}");
            continue;
        }
    }

    // ウィンドウクローズ: シャットダウンを要求し、全スレッドをjoinして終了する。
    shutdown.store(true, Ordering::Relaxed);
    let _ = capture_handle.join();
    let _ = audio_handle.join();
    let _ = inference_handle.join();

    Ok(())
}
