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

        for (position, params) in self.cons.iter_mut() {
            let amplifier = params.amplifier;
            let mut pos = *position;
            let mut buffer_index = 0;
            let total_samples = data.len();
            
            let chunks = total_samples / 4;
            for _ in 0..chunks {
                let frame0 = self.clip.sample(pos);
                let frame1 = self.clip.sample(pos + delta);
                let frame2 = self.clip.sample(pos + delta * 2.0);
                let frame3 = self.clip.sample(pos + delta * 3.0);
                
                if let (Some(f0), Some(f1), Some(f2), Some(f3)) = (frame0, frame1, frame2, frame3) {
                    let amps = [amplifier; 4];
                    let samples = [
                        f0.avg() * amps[0],
                        f1.avg() * amps[1],
                        f2.avg() * amps[2],
                        f3.avg() * amps[3],
                    ];
                    
                    for (i, sample) in samples.iter().enumerate() {
                        data[buffer_index + i] += sample;
                    }

                    buffer_index += 4;
                    pos += delta * 4.0;
                } else {
                    break;
                }
            }
            
            let remaining = total_samples - buffer_index;
            for i in 0..remaining {
                if let Some(frame) = self.clip.sample(pos) {
                    data[buffer_index + i] += frame.avg() * amplifier;
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

        for (position, params) in self.cons.iter_mut() {
            let amplifier = params.amplifier;
            let mut pos = *position;
            let total_samples = data.len();
            let total_frames = total_samples / 2;
            let mut frame_index = 0;
            
            let chunks = total_frames / 4;
            for _ in 0..chunks {
                let frames = [
                    self.clip.sample(pos),
                    self.clip.sample(pos + delta),
                    self.clip.sample(pos + delta * 2.0),
                    self.clip.sample(pos + delta * 3.0),
                ];
                
                if let [Some(f0), Some(f1), Some(f2), Some(f3)] = frames {
                    let base_index = frame_index * 2;
                    
                    data[base_index] += f0.0 * amplifier;
                    data[base_index + 2] += f1.0 * amplifier;
                    data[base_index + 4] += f2.0 * amplifier;
                    data[base_index + 6] += f3.0 * amplifier;
                    
                    data[base_index + 1] += f0.1 * amplifier;
                    data[base_index + 3] += f1.1 * amplifier;
                    data[base_index + 5] += f2.1 * amplifier;
                    data[base_index + 7] += f3.1 * amplifier;

                    frame_index += 4;
                    pos += delta * 4.0;
                } else {
                    break;
                }
            }

            // 处理剩余帧（0-3帧）
            let remaining = total_frames - frame_index;
            for i in 0..remaining {
                if let Some(frame) = self.clip.sample(pos) {
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