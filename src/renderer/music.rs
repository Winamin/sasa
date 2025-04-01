use crate::{buffer_is_full, AudioClip, Frame, Renderer};
use anyhow::{Context, Result};
use ringbuf::{HeapConsumer, HeapProducer, HeapRb};
use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    Arc, Weak,
};
use crossbeam_channel::{bounded, Receiver, Sender};

#[derive(Debug, Clone)]
pub struct MusicParams {
    pub loop_mix_time: f32,
    pub amplifier: f32,
    pub playback_rate: f32,
    pub command_buffer_size: usize,
}
impl Default for MusicParams {
    fn default() -> Self {
        Self {
            loop_mix_time: -1.,
            amplifier: 1.,
            playback_rate: 1.,
            command_buffer_size: 16,
        }
    }
}

#[repr(align(64))]
struct AtomicState {
    position: AtomicU32,
    paused: AtomicBool,
}

impl Default for AtomicState {
    fn default() -> Self {
        Self {
            position: AtomicU32::default(),
            paused: AtomicBool::new(true),
        }
    }
}

enum MusicCommand {
    Pause,
    Resume,
    SetAmplifier(f32),
    SeekTo(f32),
    SetLowPass(f32),
    FadeIn(f32),
    FadeOut(f32),
}
pub(crate) struct MusicRenderer {
    clip: AudioClip,
    settings: MusicParams,
    state: Weak<AtomicState>,
    cons: HeapConsumer<MusicCommand>,
    paused: bool,
    index: usize,
    last_sample_rate: u32,
    low_pass: f32,
    last_output: Frame,

    fade_time: i32,
    fade_current: i32,

    buffer_cache: Box<[f32; 4096]>,
    #[repr(align(64))]
    frame_buffer: [(f32, f32); 128],

    #[derive(Default)]
    cache: AudioCache,
}

#[derive(Default)]
struct AudioCache {
    last_position: f32,
    last_frame: Frame,
    sample_rate_reciprocal: f32,
}

impl MusicRenderer {
    fn prepare(&mut self, sample_rate: u32) {
        if self.last_sample_rate != sample_rate {
            let factor = sample_rate as f32 / self.last_sample_rate as f32;
            self.index = (self.index as f32 * factor).round() as _;
            self.last_sample_rate = sample_rate;
            self.fade_time = (self.fade_time as f32 * factor).round() as _;
            self.fade_current = (self.fade_current as f32 * factor).round() as _;
        }
        for cmd in self.cons.pop_iter() {
            match cmd {
                MusicCommand::Pause => {
                    self.paused = true;
                    if let Some(state) = self.state.upgrade() {
                        state.paused.store(true, Ordering::SeqCst);
                    }
                }
                MusicCommand::Resume => {
                    self.paused = false;
                    if let Some(state) = self.state.upgrade() {
                        state.paused.store(false, Ordering::SeqCst);
                    }
                }
                MusicCommand::SetAmplifier(amp) => {
                    self.settings.amplifier = amp;
                }
                MusicCommand::SeekTo(position) => {
                    self.index = (position * sample_rate as f32 / self.settings.playback_rate)
                        .round() as usize;
                }
                MusicCommand::SetLowPass(low_pass) => {
                    self.low_pass = low_pass;
                }
                MusicCommand::FadeIn(time) => {
                    if self.paused {
                        self.paused = false;
                        if let Some(state) = self.state.upgrade() {
                            state.paused.store(false, Ordering::SeqCst);
                        }
                    }
                    self.fade_time = (time * sample_rate as f32).round() as _;
                    self.fade_current = 0;
                }
                MusicCommand::FadeOut(time) => {
                    self.fade_time = (-time * sample_rate as f32).round() as _;
                    self.fade_current = 0;
                }
            }
        }
    }

    #[inline(always)]
    fn frame(&mut self, position: f32, delta: f32) -> Option<Frame> {
        let clip_length = self.clip.length();
        let s = &self.settings;
        let amp = s.amplifier * self.calculate_fade_amp();
        
        if position >= clip_length {
            return self.handle_loop_case(position, delta, amp);
        }
        
        self.sample_and_amplify(position, amp)
    }

    #[inline(always)]
    fn calculate_fade_amp(&self) -> f32 {
        if self.fade_time == 0 {
            return 1.0;
        }
        self.fade_current as f32 / self.fade_time.abs() as f32
    }

    #[inline]
    fn frame(&mut self, position: f32, delta: f32) -> Option<Frame> {
        let s = &self.settings;
        if let Some(mut frame) = self.clip.sample(position) {
            if s.loop_mix_time >= 0. {
                let pos = position + s.loop_mix_time - self.clip.length();
                if pos >= 0. {
                    if let Some(new_frame) = self.clip.sample(pos) {
                        frame = frame + new_frame;
                    }
                }
            }
            self.index += 1;
            let mut amp = s.amplifier;
            if self.fade_time != 0 {
                if self.fade_time > 0 {
                    self.fade_current += 1;
                    if self.fade_current >= self.fade_time {
                        self.fade_time = 0;
                    } else {
                        amp *= self.fade_current as f32 / self.fade_time as f32;
                    }
                } else {
                    self.fade_current -= 1;
                    if self.fade_current <= self.fade_time {
                        self.fade_time = 0;
                        self.paused = true;
                        if let Some(state) = self.state.upgrade() {
                            state.paused.store(true, Ordering::SeqCst);
                        }
                        return None;
                    } else {
                        amp *= 1. - self.fade_current as f32 / self.fade_time as f32;
                    }
                }
            }
            Some(frame * amp)
        } else if s.loop_mix_time >= 0. {
            let position = position - self.clip.length() + s.loop_mix_time;
            self.index = (position / delta).round() as _;
            Some(if let Some(frame) = self.clip.sample(position) {
                frame * s.amplifier
            } else {
                Frame::default()
            })
        } else {
            self.paused = true;
            None
        }
    }

    #[inline]
    fn position(&self, delta: f32) -> f32 {
        self.index as f32 * delta
    }

    #[inline(always)]
    fn update_and_get(&mut self, frame: Frame) -> Frame {
        self.last_output = self.last_output * self.low_pass + frame * (1. - self.low_pass);
        self.last_output
    }
}

impl Renderer for MusicRenderer {
    fn alive(&self) -> bool {
        self.state.strong_count() != 0
    }

    fn render_mono(&mut self, sample_rate: u32, data: &mut [f32]) {
        self.prepare(sample_rate);
        if !self.paused {
            let delta = 1. / sample_rate as f64 * self.settings.playback_rate as f64;
            let mut position = self.index as f64 * delta;
            for sample in data.iter_mut() {
                if let Some(frame) = self.frame(position as f32, delta as f32) {
                    *sample += self.update_and_get(frame).avg();
                } else {
                    break;
                }
                position += delta;
            }
            if let Some(state) = self.state.upgrade() {
                state
                    .position
                    .store(self.position(delta as f32).to_bits(), Ordering::SeqCst);
            }
        }
    }

    fn render_stereo(&mut self, sample_rate: u32, data: &mut [f32]) {
        self.prepare(sample_rate);
        if !self.paused {
            let delta = 1. / sample_rate as f64 * self.settings.playback_rate as f64;
            let mut position = self.index as f64 * delta;
            for sample in data.chunks_exact_mut(2) {
                if let Some(frame) = self.frame(position as f32, delta as f32) {
                    let frame = self.update_and_get(frame);
                    sample[0] += frame.0;
                    sample[1] += frame.1;
                } else {
                    break;
                }
                position += delta;
            }
            if let Some(state) = self.state.upgrade() {
                state
                    .position
                    .store(self.position(delta as f32).to_bits(), Ordering::SeqCst);
            }
        }
    }
}

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

impl MusicRenderer {
    #[inline(always)]
    unsafe fn render_stereo_simd(&mut self, data: &mut [f32], sample_rate: u32) {
        if is_x86_feature_detected!("avx2") {
            for chunk in data.chunks_exact_mut(16) {
                let position = self.index as f64 * self.delta(sample_rate);
                let frame = self.frame(position as f32, self.delta(sample_rate) as f32);
                
                let samples = _mm256_loadu_ps(chunk.as_ptr());
                let processed = self.process_frame_avx2(samples, frame);
                _mm256_storeu_ps(chunk.as_mut_ptr(), processed);
                
                self.index += 8;
            }
        } else if is_x86_feature_detected!("sse4.1") {
            self.render_stereo_sse(data, sample_rate);
        }
    }
}

pub struct Music {
    command_sender: Sender<MusicCommand>,
    state: Arc<AtomicState>,
}

impl Music {
    pub(crate) fn new(clip: AudioClip, settings: MusicParams) -> (Music, MusicRenderer) {
        let (prod, cons) = HeapRb::new(settings.command_buffer_size).split();
        let arc = Arc::default();
        let renderer = MusicRenderer {
            clip,
            settings,
            state: Arc::downgrade(&arc),
            cons,
            paused: true,
            index: 0,
            last_sample_rate: 1,
            low_pass: 0.,
            last_output: Frame(0., 0.),

            fade_time: 0,
            fade_current: 0,

            buffer_cache: Box::new([0.0; 4096]),
            frame_buffer: [(0.0, 0.0); 128],

            cache: AudioCache::default(),
        };
        (Self { arc, prod }, renderer)
    }

    pub fn play(&mut self) -> Result<()> {
        self.prod
            .push(MusicCommand::Resume)
            .map_err(buffer_is_full)
            .context("play music")
    }

    pub fn pause(&mut self) -> Result<()> {
        self.prod
            .push(MusicCommand::Pause)
            .map_err(buffer_is_full)
            .context("pause")
    }

    pub fn paused(&mut self) -> bool {
        self.arc.paused.load(Ordering::SeqCst)
    }

    pub fn set_amplifier(&mut self, amp: f32) -> Result<()> {
        self.prod
            .push(MusicCommand::SetAmplifier(amp))
            .map_err(buffer_is_full)
            .context("set amplifier")
    }

    pub fn seek_to(&mut self, position: f32) -> Result<()> {
        self.prod
            .push(MusicCommand::SeekTo(position))
            .map_err(buffer_is_full)
            .context("seek to")
    }

    pub fn set_low_pass(&mut self, low_pass: f32) -> Result<()> {
        self.prod
            .push(MusicCommand::SetLowPass(low_pass))
            .map_err(buffer_is_full)
            .context("set low pass")
    }

    pub fn fade_in(&mut self, time: f32) -> Result<()> {
        self.prod
            .push(MusicCommand::FadeIn(time))
            .map_err(buffer_is_full)
            .context("fade in")
    }

    pub fn fade_out(&mut self, time: f32) -> Result<()> {
        self.prod
            .push(MusicCommand::FadeOut(time))
            .map_err(buffer_is_full)
            .context("fade out")
    }

    pub fn position(&self) -> f32 {
        f32::from_bits(self.arc.position.load(Ordering::SeqCst))
    }
}
