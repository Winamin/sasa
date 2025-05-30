use crate::{buffer_is_full, AudioClip, Frame, Renderer};
use anyhow::{Context, Result};
use ringbuf::{HeapConsumer, HeapProducer, HeapRb};
use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    Arc, Weak,
};

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

struct SharedState {
    position: AtomicU32, // float in bits
    paused: AtomicBool,
}
impl Default for SharedState {
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
    state: Weak<SharedState>,
    cons: HeapConsumer<MusicCommand>,
    paused: bool,
    index: usize,
    last_sample_rate: u32,
    low_pass: f32,
    last_output: Frame,
    
    fade_type: u8, // 0: 无, 1: 淡入, 2: 淡出
    fade_samples: u32,
    fade_current: u32,
}

impl MusicRenderer {
    fn prepare(&mut self, sample_rate: u32) {
        if self.last_sample_rate != sample_rate {
            let factor = sample_rate as f32 / self.last_sample_rate as f32;
            self.index = (self.index as f32 * factor).round() as _;
            self.last_sample_rate = sample_rate;
            self.fade_samples = (self.fade_samples as f32 * factor).round() as _;
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
                    self.fade_type = 1;
                    self.fade_samples = (time * sample_rate as f32).round() as _;
                    self.fade_current = 0;
                }
                MusicCommand::FadeOut(time) => {
                    self.fade_type = 2;
                    self.fade_samples = (time * sample_rate as f32).round() as _;
                    self.fade_current = 0;
                }
            }
        }
    }

    #[inline]
    fn get_amplifier(&mut self) -> f32 {
        if self.fade_type == 0 {
            return self.settings.amplifier;
        }

        self.fade_current += 1;
        let amp = match self.fade_type {
            1 => self.settings.amplifier * (self.fade_current as f32 / self.fade_samples as f32),
            2 => {
                let progress = self.fade_current as f32 / self.fade_samples as f32;
                if progress >= 1.0 {
                    self.fade_type = 0;
                    self.paused = true;
                    if let Some(state) = self.state.upgrade() {
                        state.paused.store(true, Ordering::SeqCst);
                    }
                    return 0.0;
                }
                self.settings.amplifier * (1.0 - progress)
            }
            _ => self.settings.amplifier,
        };

        if self.fade_current >= self.fade_samples {
            self.fade_type = 0;
        }

        amp
    }

    #[inline]
    fn get_frame(&mut self, position: f32) -> Option<Frame> {
        let amp = self.get_amplifier();
        let loop_mix_time = self.settings.loop_mix_time;
        let playback_rate = self.settings.playback_rate;

        if let Some(mut frame) = self.clip.sample(position) {
            if loop_mix_time >= 0. {
                let pos = position + loop_mix_time - self.clip.length();
                if pos >= 0. {
                    if let Some(new_frame) = self.clip.sample(pos) {
                        frame = frame + new_frame;
                    }
                }
            }
            self.index += 1;
            Some(frame * amp)
        } else if loop_mix_time >= 0. {
            let position = position - self.clip.length() + loop_mix_time;
            self.index = (position * self.last_sample_rate as f32 / playback_rate)
                .round() as usize;
            Some(if let Some(frame) = self.clip.sample(position) {
                frame * amp
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
        let alpha = 1.0 - self.low_pass;
        self.last_output.0 = self.low_pass * self.last_output.0 + alpha * frame.0;
        self.last_output.1 = self.low_pass * self.last_output.1 + alpha * frame.1;
        self.last_output
    }
    
    fn process_block(&mut self, start_pos: f64, delta: f64, samples: &mut [f32], stereo: bool) {
        let block_size = 4; // 4帧块处理
        let mut pos = start_pos;
        let mut i = 0;

        while i < samples.len() {
            let remaining = samples.len() - i;
            let to_process = if stereo { remaining.min(block_size * 2) } else { remaining.min(block_size) };
            
            let mut frames = [Frame::default(); 4];
            let mut valid_count = 0;

            for j in 0..(to_process / if stereo { 2 } else { 1 }) {
                if let Some(frame) = self.get_frame(pos as f32) {
                    frames[j] = self.update_and_get(frame);
                    valid_count += 1;
                    pos += delta;
                } else {
                    break;
                }
            }
            
            if stereo {
                for j in 0..valid_count {
                    samples[i + j * 2] += frames[j].0;
                    samples[i + j * 2 + 1] += frames[j].1;
                }
                i += valid_count * 2;
            } else {
                for j in 0..valid_count {
                    samples[i + j] += frames[j].avg();
                }
                i += valid_count;
            }
            
            if valid_count < to_process / if stereo { 2 } else { 1 } {
                break;
            }
        }

        if let Some(state) = self.state.upgrade() {
            state
                .position
                .store(self.position(delta as f32).to_bits(), Ordering::SeqCst);
        }
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
            let start_pos = self.index as f64 * delta;
            self.process_block(start_pos, delta, data, false);
        }
    }

    fn render_stereo(&mut self, sample_rate: u32, data: &mut [f32]) {
        self.prepare(sample_rate);
        if !self.paused {
            let delta = 1. / sample_rate as f64 * self.settings.playback_rate as f64;
            let start_pos = self.index as f64 * delta;
            self.process_block(start_pos, delta, data, true);
        }
    }
}

pub struct Music {
    arc: Arc<SharedState>,
    prod: HeapProducer<MusicCommand>,
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

            // 淡入淡出状态
            fade_type: 0,
            fade_samples: 0,
            fade_current: 0,
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