use crate::{buffer_is_full, AudioClip, Renderer};
use anyhow::{Context, Result};
use ringbuf::{HeapConsumer, HeapProducer, HeapRb};
use std::sync::{Arc, Weak};

#[derive(Debug, Clone)]
pub struct PlaySfxParams {
    pub amplifier: f32,
}

impl Default for PlaySfxParams {
    fn default() -> Self {
        Self { amplifier: 1. }
    }
}

pub(crate) struct SfxRenderer {
    clip: AudioClip,
    arc: Weak<()>,
    cons: HeapConsumer<(f32, PlaySfxParams)>,
}

impl Renderer for SfxRenderer {
    fn alive(&self) -> bool {
        !self.cons.is_empty() || self.arc.strong_count() != 0
    }

    fn render_mono(&mut self, sample_rate: u32, data: &mut [f32]) {
        let delta = 1. / sample_rate as f32;
        let mut pop_count = 0;
        let clip = &self.clip;

        for (position, params) in self.cons.iter_mut() {
            let amplifier = params.amplifier;
            let mut pos = *position;
            let mut buffer_index = 0;
            let total_samples = data.len();

            // Process samples in batches of 8
            let chunks = total_samples / 8;
            for _ in 0..chunks {
                let mut valid = true;
                let mut samples = [0.0; 8];

                // Unroll the sampling loop
                for i in 0..8 {
                    if let Some(frame) = clip.sample(pos + delta * i as f32) {
                        samples[i] = (frame.0 + frame.1) * 0.5 * amplifier;
                    } else {
                        valid = false;
                        break;
                    }
                }

                if valid {
                    // Batch write to output buffer
                    for i in 0..8 {
                        data[buffer_index + i] += samples[i];
                    }
                    buffer_index += 8;
                    pos += delta * 8.0;
                } else {
                    break;
                }
            }

            // Process remaining samples (0-7)
            let remaining = total_samples - buffer_index;
            for i in 0..remaining {
                if let Some(frame) = clip.sample(pos) {
                    data[buffer_index + i] += (frame.0 + frame.1) * 0.5 * amplifier;
                    pos += delta;
                } else {
                    pop_count += 1;
                    break;
                }
            }

            if pop_count == 0 {
                *position = pos;
            }
        }

        unsafe {
            self.cons.advance(pop_count);
        }
    }

    fn render_stereo(&mut self, sample_rate: u32, data: &mut [f32]) {
        let delta = 1. / sample_rate as f32;
        let mut pop_count = 0;
        let clip = &self.clip;

        for (position, params) in self.cons.iter_mut() {
            let amplifier = params.amplifier;
            let mut pos = *position;
            let total_samples = data.len();
            let total_frames = total_samples / 2;
            let mut frame_index = 0;

            // Process frames in batches of 8
            let chunks = total_frames / 8;
            for _ in 0..chunks {
                let mut frames = [(0.0, 0.0); 8];
                let mut valid = true;

                // Unroll the sampling loop
                for i in 0..8 {
                    if let Some(frame) = clip.sample(pos + delta * i as f32) {
                        frames[i] = (frame.0, frame.1);
                    } else {
                        valid = false;
                        break;
                    }
                }

                if valid {
                    let base_index = frame_index * 2;
                    // Write batch to output buffer
                    for i in 0..8 {
                        let idx = base_index + i * 2;
                        data[idx] += frames[i].0 * amplifier;
                        data[idx + 1] += frames[i].1 * amplifier;
                    }
                    frame_index += 8;
                    pos += delta * 8.0;
                } else {
                    break;
                }
            }

            // Process remaining frames (0-7)
            let remaining = total_frames - frame_index;
            for i in 0..remaining {
                if let Some(frame) = clip.sample(pos) {
                    let idx = (frame_index + i) * 2;
                    data[idx] += frame.0 * amplifier;
                    data[idx + 1] += frame.1 * amplifier;
                    pos += delta;
                } else {
                    pop_count += 1;
                    break;
                }
            }

            if pop_count == 0 {
                *position = pos;
            }
        }

        unsafe {
            self.cons.advance(pop_count);
        }
    }
}

pub struct Sfx {
    _arc: Arc<()>,
    prod: HeapProducer<(f32, PlaySfxParams)>,
}

impl Sfx {
    pub(crate) fn new(clip: AudioClip, buffer_size: Option<usize>) -> (Sfx, SfxRenderer) {
        let (prod, cons) = HeapRb::new(buffer_size.unwrap_or(4096)).split();
        let arc = Arc::new(());
        let renderer = SfxRenderer {
            clip,
            arc: Arc::downgrade(&arc),
            cons,
        };
        (Self { _arc: arc, prod }, renderer)
    }

    pub fn play(&mut self, params: PlaySfxParams) -> Result<()> {
        self.prod
            .push((0., params))
            .map_err(buffer_is_full)
            .context("play sfx")
    }
}