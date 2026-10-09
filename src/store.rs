//! The open picture + background decoding for cce-image.
//!
//! A picture is sized up front from its header (`image::image_dimensions`),
//! so layout never waits on a decode. The decode runs on a worker thread,
//! which uploads RGBA via `cce_ui::vk::upload_rgba` (the upload queue is
//! thread-safe) and notifies the app over the calloop channel so the engine
//! wakes and repaints.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use crate::Message;

/// Extensions the `image` crate is built to decode (keep in sync with the
/// feature list in Cargo.toml).
pub const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff", "ico"];

/// Largest bitmap edge we'll upload; bigger sources are downscaled.
const MAX_DIM: u32 = 8192;

pub fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

/// The open file and its size in source pixels.
pub struct Picture {
    pub path: PathBuf,
    pub w: f64,
    pub h: f64,
}

impl Picture {
    pub fn load(path: &Path) -> Result<Self, String> {
        if !is_image(path) {
            return Err("unsupported file type".to_string());
        }
        let (w, h) = image::image_dimensions(path).map_err(|e| e.to_string())?;
        Ok(Self { path: path.to_path_buf(), w: w as f64, h: h as f64 })
    }
}

struct Job {
    generation: u64,
    path: PathBuf,
    /// User rotation in quarter turns cw, applied to the pixels.
    quarter_turns: u8,
}

enum State {
    Empty,
    Pending,
    Ready(u32),
    Failed,
}

/// The one GPU image on screen. `reset()` bumps the generation so a late
/// decode of a previous file/rotation is freed on arrival instead of shown.
pub struct ImageStore {
    state: State,
    queue: mpsc::Sender<Job>,
    generation: u64,
}

impl ImageStore {
    pub fn new(notify: calloop::channel::Sender<Message>) -> Self {
        let (queue, rx) = mpsc::channel::<Job>();
        std::thread::spawn(move || worker(rx, notify));
        Self { state: State::Empty, queue, generation: 0 }
    }

    pub fn reset(&mut self) {
        if let State::Ready(image) = std::mem::replace(&mut self.state, State::Empty) {
            cce_ui::vk::free_image(image);
        }
        self.generation += 1;
    }

    /// The picture's GPU image if resident; otherwise queues a decode (once)
    /// and returns None.
    pub fn ensure(&mut self, pic: &Picture, quarter_turns: u8) -> Option<u32> {
        match self.state {
            State::Ready(image) => Some(image),
            State::Pending | State::Failed => None,
            State::Empty => {
                self.state = State::Pending;
                let _ = self.queue.send(Job { generation: self.generation, path: pic.path.clone(), quarter_turns });
                None
            }
        }
    }

    pub fn complete(&mut self, generation: u64, result: Option<u32>) {
        if generation != self.generation {
            if let Some(image) = result {
                cce_ui::vk::free_image(image);
            }
            return;
        }
        self.state = match result {
            Some(image) => State::Ready(image),
            None => State::Failed,
        };
    }
}

fn worker(rx: mpsc::Receiver<Job>, notify: calloop::channel::Sender<Message>) {
    // Only the newest job matters: opening files faster than they decode
    // (holding an arrow key) would otherwise decode every one in between.
    // A job is only queued after a `reset`, so any older one is stale.
    while let Ok(mut job) = rx.recv() {
        while let Ok(newer) = rx.try_recv() {
            job = newer;
        }
        let result = decode(&job).map_err(|e| log::warn!("{}: {e}", job.path.display())).ok();
        if notify.send(Message::Decoded { generation: job.generation, result }).is_err() {
            return;
        }
    }
}

fn decode(job: &Job) -> Result<u32, String> {
    let img = image::open(&job.path).map_err(|e| e.to_string())?;
    let mut rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    if w.max(h) > MAX_DIM {
        let s = MAX_DIM as f64 / w.max(h) as f64;
        let (nw, nh) = (((w as f64 * s) as u32).max(1), ((h as f64 * s) as u32).max(1));
        rgba = image::imageops::resize(&rgba, nw, nh, image::imageops::FilterType::Triangle);
    }
    match job.quarter_turns % 4 {
        1 => rgba = image::imageops::rotate90(&rgba),
        2 => rgba = image::imageops::rotate180(&rgba),
        3 => rgba = image::imageops::rotate270(&rgba),
        _ => {}
    }
    let (w, h) = rgba.dimensions();
    Ok(cce_ui::vk::upload_rgba(rgba.into_raw(), w, h))
}
