use crossbeam_channel::{Receiver, Sender, bounded};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;
use tracing::error;

use crate::hardware::{FrameBuffer, VideoSource};

const SINGLE_SLOT_LATEST_FRAME_ONLY: usize = 1;

fn publish_latest_frame_dropping_lagging(
    sender: &Sender<FrameBuffer>,
    receiver_drain_handle: &Receiver<FrameBuffer>,
    new_frame: FrameBuffer,
) {
    if let Err(crossbeam_channel::TrySendError::Full(rejected_frame)) = sender.try_send(new_frame) {
        let _ = receiver_drain_handle.try_recv();
        let _ = sender.try_send(rejected_frame);
    }
}

/// キャプチャサービス。
///
/// 具体的な `VideoSource`(例: `NokhwaCapture`)は呼び出し側が注入するため、
/// キャプチャ手段の追加は新しい `VideoSource` 実装の追加だけで対応できる。
pub struct CaptureService {
    source: Box<dyn VideoSource>,
    ml_sample_interval_frames: u32,
}

impl CaptureService {
    pub fn new(source: Box<dyn VideoSource>, ml_sample_interval_frames: u32) -> Self {
        Self {
            source,
            ml_sample_interval_frames: ml_sample_interval_frames.max(1),
        }
    }

    /// キャプチャスレッドを起動する。
    ///
    /// 設計メモ: 映像パス(1スロットch、最新フレーム優先)と音声パス(リングバッファ)は
    /// 共有クロックを持たない(独立したタイムスタンプソースがない)。
    /// 今のパススルー用途では問題にならないが、将来ML検出結果と音声イベントを
    /// 突き合わせるときは、共有クロック(例: 取得時の `Instant` 刻印)を先に要る。
    pub fn spawn_loop(
        self,
        shutdown: Arc<AtomicBool>,
    ) -> (Receiver<FrameBuffer>, Receiver<FrameBuffer>, JoinHandle<()>) {
        let (tx_display, rx_display) = bounded::<FrameBuffer>(SINGLE_SLOT_LATEST_FRAME_ONLY);
        let (tx_ml, rx_ml) = bounded::<FrameBuffer>(SINGLE_SLOT_LATEST_FRAME_ONLY);

        let rx_display_drain_handle = rx_display.clone();
        let rx_ml_drain_handle = rx_ml.clone();

        let handle = thread::spawn(move || {
            let mut source = self.source;

            let mut frames_since_last_ml_sample = 0u32;

            while !shutdown.load(Ordering::Relaxed) {
                let frame_buffer = match source.capture_frame() {
                    Ok(buffer) => buffer,
                    Err(e) => {
                        error!("Capture frame error: {e}");
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                };

                publish_latest_frame_dropping_lagging(
                    &tx_display,
                    &rx_display_drain_handle,
                    Arc::clone(&frame_buffer),
                );

                frames_since_last_ml_sample += 1;
                if frames_since_last_ml_sample >= self.ml_sample_interval_frames {
                    frames_since_last_ml_sample = 0;
                    publish_latest_frame_dropping_lagging(
                        &tx_ml,
                        &rx_ml_drain_handle,
                        frame_buffer,
                    );
                }
            }
        });

        (rx_display, rx_ml, handle)
    }
}
