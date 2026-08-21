use super::{CropArea, PixelCropArea};
use crate::hardware::FrameBuffer;
use anyhow::Result;
use crossbeam_channel::Receiver;
use minifb::{Key, Window, WindowOptions};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

const STEP: f32 = 0.0025; // 移動・リサイズの相対ステップ

pub struct DisplayWindow {
    window: Window,
    width: usize,
    height: usize,
    current_frame: FrameBuffer,
    render_buffer: Vec<u32>,
    last_input_time: Instant,
    show_debug_frame: bool,
}

impl DisplayWindow {
    pub fn open_uncapped(title: &str, resolution: (usize, usize)) -> Result<Self> {
        let (width, height) = resolution;
        let mut window = Window::new(title, width, height, WindowOptions::default())?;

        const UNCAPPED_FPS: usize = 0;
        window.set_target_fps(UNCAPPED_FPS);

        Ok(Self {
            window,
            width,
            height,
            current_frame: Arc::new(vec![0u32; width * height]),
            render_buffer: vec![0u32; width * height],
            last_input_time: Instant::now(),
            show_debug_frame: cfg!(debug_assertions),
        })
    }

    pub fn is_open(&self) -> bool {
        self.window.is_open()
    }

    fn handle_input(&mut self, crop_area: &Arc<RwLock<CropArea>>) {
        if cfg!(debug_assertions) && self.window.is_key_pressed(Key::D, minifb::KeyRepeat::No) {
            self.show_debug_frame = !self.show_debug_frame;
            println!("[Debug Frame] Visibility: {}", self.show_debug_frame);
            return;
        }

        if self.last_input_time.elapsed() < Duration::from_millis(50) {
            return;
        }

        let mut crop = crop_area.write().unwrap();
        let shift =
            self.window.is_key_down(Key::LeftShift) || self.window.is_key_down(Key::RightShift);

        let mut changed = false;

        if shift {
            if self.window.is_key_down(Key::Left) {
                crop.width -= STEP;
                changed = true;
            }
            if self.window.is_key_down(Key::Right) {
                crop.width += STEP;
                changed = true;
            }
            if self.window.is_key_down(Key::Up) {
                crop.height -= STEP;
                changed = true;
            }
            if self.window.is_key_down(Key::Down) {
                crop.height += STEP;
                changed = true;
            }
        } else {
            if self.window.is_key_down(Key::Left) {
                crop.x -= STEP;
                changed = true;
            }
            if self.window.is_key_down(Key::Right) {
                crop.x += STEP;
                changed = true;
            }
            if self.window.is_key_down(Key::Up) {
                crop.y -= STEP;
                changed = true;
            }
            if self.window.is_key_down(Key::Down) {
                crop.y += STEP;
                changed = true;
            }
        }

        if changed {
            crop.clamp();
            self.last_input_time = Instant::now();
            println!(
                "[Crop Box] x: {:.4}, y: {:.4}, w: {:.4}, h: {:.4}",
                crop.x, crop.y, crop.width, crop.height
            );
        }
    }

    fn drain_queue_and_get_latest_frame(&mut self, rx_frame: &Receiver<FrameBuffer>) -> bool {
        let mut received_new_frame = false;
        while let Ok(latest_frame) = rx_frame.try_recv() {
            self.current_frame = latest_frame;
            received_new_frame = true;
        }
        received_new_frame
    }

    pub fn render_latest(
        &mut self,
        rx_frame: &Receiver<FrameBuffer>,
        crop_area: &Arc<RwLock<CropArea>>,
    ) -> Result<()> {
        self.handle_input(crop_area);

        let has_new_frame = self.drain_queue_and_get_latest_frame(rx_frame);

        if has_new_frame {
            self.render_buffer.copy_from_slice(&self.current_frame);

            if cfg!(debug_assertions) && self.show_debug_frame {
                let crop = crop_area.read().unwrap().to_pixels(self.width, self.height);
                draw_red_box(&mut self.render_buffer, self.width, self.height, &crop);
            }

            self.window
                .update_with_buffer(&self.render_buffer, self.width, self.height)?;
        } else {
            self.window.update();
            thread::sleep(Duration::from_millis(1));
        }

        Ok(())
    }
}

fn draw_red_box(buffer: &mut [u32], img_w: usize, img_h: usize, crop: &PixelCropArea) {
    const RED_COLOR: u32 = 0x00FF_0000;
    let thickness = 3;

    let x1 = crop.x;
    let y1 = crop.y;
    let x2 = (crop.x + crop.width).min(img_w);
    let y2 = (crop.y + crop.height).min(img_h);

    for t in 0..thickness {
        for x in x1..x2 {
            if y1 + t < img_h {
                buffer[(y1 + t) * img_w + x] = RED_COLOR;
            }
            if y2.saturating_sub(1 + t) < img_h {
                buffer[(y2.saturating_sub(1 + t)) * img_w + x] = RED_COLOR;
            }
        }
        for y in y1..y2 {
            if x1 + t < img_w {
                buffer[y * img_w + (x1 + t)] = RED_COLOR;
            }
            if x2.saturating_sub(1 + t) < img_w {
                buffer[y * img_w + x2.saturating_sub(1 + t)] = RED_COLOR;
            }
        }
    }
}
