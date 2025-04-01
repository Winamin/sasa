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
            for sample in data.iter_mut() {
                if let Some(frame) = self.clip.sample(*position) {
                    *sample += frame.avg() * params.amplifier;
                } else {
                    pop_count += 1;
                    break;
                }
                *position += delta;
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
            for sample in data.chunks_exact_mut(2) {
                if let Some(frame) = self.clip.sample(*position) {
                    sample[0] += frame.0 * params.amplifier;
                    sample[1] += frame.1 * params.amplifier;
                } else {
                    pop_count += 1;
                    break;
                }
                *position += delta;
            }
        }
        unsafe {
            self.cons.advance(pop_count);
        }
    }
}

<<<<<<< HEAD
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

impl SfxRenderer {
    #[inline(always)]
    unsafe fn render_stereo_simd(&mut self, sample_rate: u32, data: &mut [f32]) {
        let delta = 1. / sample_rate as f32;
        let mut pop_count = 0;
        
        if is_x86_feature_detected!("avx2") {
            for (position, params) in self.cons.iter_mut() {
                let amp = _mm256_set1_ps(params.amplifier);
                for chunk in data.chunks_exact_mut(8) {
                    if let Some(frame) = self.clip.sample(*position) {
                        let frame_vec = _mm256_set_ps(
                            frame.0, frame.1, frame.0, frame.1,
                            frame.0, frame.1, frame.0, frame.1
                        );
                        let samples = _mm256_loadu_ps(chunk.as_ptr());
                        let result = _mm256_fmadd_ps(frame_vec, amp, samples);
                        _mm256_storeu_ps(chunk.as_mut_ptr(), result);
                    } else {
                        pop_count += 1;
                        break;
                    }
                    *position += delta * 4.0;
                }
            }
        } else if is_x86_feature_detected!("sse2") {
            self.render_stereo(sample_rate, data);
        } else {
            self.render_stereo(sample_rate, data);
        }
        
        unsafe {
            self.cons.advance(pop_count);
        }
    }

    #[inline(always)]
    fn process_batch(&mut self, data: &mut [f32], sample_rate: u32) {
        const BATCH_SIZE: usize = 64;
        let delta = 1. / sample_rate as f32;
        
        for chunk in data.chunks_exact_mut(BATCH_SIZE) {
            unsafe{
                std::intrinsics::prefetch_read_data(chunk.as_ptr(), 3);
            }
            self.process_chunk(chunk, delta);
        }
    }

    #[inline(always)]
    fn process_chunk(&mut self, chunk: &mut [f32], delta: f32) {
        if self.sample_cache.last_position == 0.0 {
            self.sample_cache.delta_reciprocal = 1.0 / delta;
        }
        
        unsafe {
            self.process_chunk_simd(chunk);
        }
    }

    #[inline(always)]
    unsafe fn process_chunk_simd(&mut self, chunk: &mut [f32]) {
        for sample in chunk.iter_mut() {
            *sample = 0.0;
        }
    }
}

=======
>>>>>>> parent of 07b2a80 (refactor: all sasa)
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
