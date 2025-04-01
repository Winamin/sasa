use crate::Backend;
use anyhow::{anyhow, Context, Result};
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    BufferSize, OutputCallbackInfo, Stream, StreamError,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use super::{BackendSetup, StateCell};

#[derive(Debug, Clone, Default)]
pub struct CpalSettings {
    pub buffer_size: Option<u32>,
}

pub struct CpalBackend {
    settings: CpalSettings,
    stream: Option<Stream>,
    broken: Arc<AtomicBool>,
    state: Option<Arc<StateCell>>,
    buffer_pool: Vec<Box<[f32]>>,
    working_buffer: Vec<f32>,
}

impl CpalBackend {
    pub fn new(settings: CpalSettings) -> Self {
        let mut backend = Self {
            settings,
            stream: None,
            broken: Arc::default(),
            state: None,
            buffer_pool: Vec::with_capacity(4),
            working_buffer: Vec::with_capacity(4096),
        };
        
        for _ in 0..4 {
            backend.buffer_pool.push(vec![0.0f32; 4096].into_boxed_slice());
        }
        backend
    }
}

impl Backend for CpalBackend {
    fn setup(&mut self, setup: BackendSetup) -> Result<()> {
        self.state = Some(Arc::new(setup.into()));
        Ok(())
    }

    fn start(&mut self) -> Result<()> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow!("no default output device is found"))?;
        let mut config = device
            .default_output_config()
            .context("cannot get output config")?
            .config();
        config.buffer_size = self
            .settings
            .buffer_size
            .map_or(BufferSize::Default, |it| BufferSize::Fixed(it));

        let broken = Arc::clone(&self.broken);
        let error_callback = move |err| {
            eprintln!("audio error: {err:?}");
            if matches!(err, StreamError::DeviceNotAvailable) {
                broken.store(true, Ordering::Relaxed);
            }
        };
        let state = Arc::clone(self.state.as_ref().unwrap());
        state.get().0.sample_rate = config.sample_rate.0;
        let stream = (if config.channels == 1 {
            device.build_output_stream(
                &config,
                move |data: &mut [f32], info: &OutputCallbackInfo| {
                    let (mixer, rec) = state.get();
                    mixer.render_mono(data);
                    let ts = info.timestamp();
                    if let Some(delay) = ts.playback.duration_since(&ts.callback) {
                        rec.push(delay.as_secs_f32());
                    }
                },
                error_callback,
            )
        } else {
            device.build_output_stream(
                &config,
                move |data: &mut [f32], info: &OutputCallbackInfo| {
                    let (mixer, rec) = state.get();
                    unsafe { render_stereo_simd(data, mixer) };
                    let ts = info.timestamp();
                    if let Some(delay) = ts.playback.duration_since(&ts.callback) {
                        rec.push(delay.as_secs_f32());
                    }
                },
                error_callback,
            )
        })
        .context("failed to build stream")?;
        stream.play()?;
        self.stream = Some(stream);
        Ok(())
    }

    fn consume_broken(&self) -> bool {
        self.broken.fetch_and(false, Ordering::Relaxed)
    }
}

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

#[inline(always)]
unsafe fn render_stereo_simd(data: &mut [f32], mixer: &mut Mixer) {
    if is_x86_feature_detected!("avx2") {
        for chunk in data.chunks_exact_mut(8) {
            let samples = _mm256_loadu_ps(chunk.as_ptr());
            let processed = mixer.process_avx2(samples);
            _mm256_storeu_ps(chunk.as_mut_ptr(), processed);
        }
    } else if is_x86_feature_detected!("sse2") {
        for chunk in data.chunks_exact_mut(4) {
            let samples = _mm_loadu_ps(chunk.as_ptr());
            let processed = mixer.process_sse2(samples);
            _mm_storeu_ps(chunk.as_mut_ptr(), processed);
        }
    }
}
