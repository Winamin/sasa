use crate::Frame;
use anyhow::{anyhow, bail, Result};
use std::{io::Cursor, sync::Arc};
use symphonia::core::{
    audio::{AudioBufferRef, Signal},
    io::MediaSourceStream,
};

#[repr(align(32))]
struct ClipInner {
    frames: Vec<Frame>,
    sample_rate: u32,
    length: f32,
    frame_count: usize,
}

pub struct AudioClip(Arc<ClipInner>);

impl Clone for AudioClip {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl AudioClip {
    pub fn from_raw(frames: Vec<Frame>, sample_rate: u32) -> Self {
        let frame_count = frames.len();
        let length = frame_count as f32 / sample_rate as f32;
        Self(Arc::new(ClipInner {
            frames,
            sample_rate,
            length,
            frame_count,
        }))
    }

    pub fn decode(data: Vec<u8>) -> Result<(Vec<Frame>, u32)> {
        const CHUNK_SIZE: usize = 4096;

        #[inline(always)]
        fn load_frames_from_buffer(
            frames: &mut Vec<Frame>,
            buffer: &symphonia::core::audio::AudioBuffer<f32>,
        ) {
            match buffer.spec().channels.count() {
                1 => {
                    let chan = buffer.chan(0);
                    let current_len = frames.len();
                    frames.reserve(chan.len());
                    unsafe {
                        frames.set_len(current_len + chan.len());
                    }
                    for (i, &sample) in chan.iter().enumerate() {
                        frames[current_len + i] = Frame(sample, sample);
                    }
                }
                _ => {
                    let left = buffer.chan(0);
                    let right = buffer.chan(1);
                    let current_len = frames.len();
                    frames.reserve(left.len());
                    unsafe {
                        frames.set_len(current_len + left.len());
                    }
                    for i in 0..left.len() {
                        frames[current_len + i] = Frame(left[i], right[i]);
                    }
                }
            }
        }

        #[inline(always)]
        fn load_frames_from_buffer_ref(
            frames: &mut Vec<Frame>,
            buffer: &AudioBufferRef,
        ) -> Result<()> {
            macro_rules! conv {
                ($buffer:ident) => {{
                    let mut dest = symphonia::core::audio::AudioBuffer::new(
                        buffer.capacity() as u64,
                        buffer.spec().clone(),
                    );
                    $buffer.convert(&mut dest);
                    load_frames_from_buffer(frames, &dest);
                }};
            }
            use AudioBufferRef::*;
            match buffer {
                F32(buffer) => load_frames_from_buffer(frames, buffer),
                U8(buffer) => conv!(buffer),
                U16(buffer) => conv!(buffer),
                U24(buffer) => conv!(buffer),
                U32(buffer) => conv!(buffer),
                S8(buffer) => conv!(buffer),
                S16(buffer) => conv!(buffer),
                S24(buffer) => conv!(buffer),
                S32(buffer) => conv!(buffer),
                F64(buffer) => conv!(buffer),
            }
            Ok(())
        }

        let codecs = symphonia::default::get_codecs();
        let probe = symphonia::default::get_probe();
        let mss = MediaSourceStream::new(Box::new(Cursor::new(data)), Default::default());
        let mut format_reader = probe
            .format(
                &Default::default(),
                mss,
                &Default::default(),
                &Default::default(),
            )?
            .format;

        let track = format_reader
            .default_track()
            .ok_or_else(|| anyhow!("default track not found"))?;

        let codec_params = &track.codec_params;
        let sample_rate = codec_params
            .sample_rate
            .ok_or_else(|| anyhow!("unknown sample rate"))?;
        
        /*
        magic????
        let estimated_frames = if let Some(n_frames) = codec_params.n_frames {
            n_frames as usize
        } else if let Some(time_base) = codec_params.time_base {
            (format_reader.duration().unwrap_or(0) as f64 * time_base.seconds_per_ts() * sample_rate as f64) as usize
        } else {
            0
        };
        
         */

        let mut frames = Vec::new();
        let mut decoder = codecs.make(codec_params, &Default::default())?;

        // 块处理解码
        let mut packets = Vec::with_capacity(CHUNK_SIZE);
        while let Ok(packet) = format_reader.next_packet() {
            packets.push(packet);
            if packets.len() >= CHUNK_SIZE {
                for packet in packets.drain(..) {
                    let buffer = match decoder.decode(&packet) {
                        Ok(buffer) => buffer,
                        Err(symphonia::core::errors::Error::DecodeError(s))
                        if s.contains("invalid main_data offset") =>
                            {
                                continue;
                            }
                        Err(err) => return Err(err.into()),
                    };
                    load_frames_from_buffer_ref(&mut frames, &buffer)?;
                }
            }
        }
        
        for packet in packets {
            let buffer = match decoder.decode(&packet) {
                Ok(buffer) => buffer,
                Err(symphonia::core::errors::Error::DecodeError(s))
                if s.contains("invalid main_data offset") =>
                    {
                        continue;
                    }
                Err(err) => return Err(err.into()),
            };
            load_frames_from_buffer_ref(&mut frames, &buffer)?;
        }

        Ok((frames, sample_rate))
    }

    #[inline]
    pub fn new(data: Vec<u8>) -> Result<Self> {
        let (frames, sample_rate) = Self::decode(data)?;
        Ok(Self::from_raw(frames, sample_rate))
    }
    
    pub fn sample(&self, position: f32) -> Option<Frame> {
        let position = position * self.0.sample_rate as f32;
        let actual_index = position as usize;
        
        if actual_index >= self.0.frame_count {
            return None;
        }

        let frame = self.0.frames[actual_index];
        let t = position - actual_index as f32;
        
        if t < f32::EPSILON {
            return Some(frame);
        }

        if actual_index + 1 >= self.0.frame_count {
            return Some(frame);
        }

        let next_frame = self.0.frames[actual_index + 1];
        Some(frame.interpolate(&next_frame, t))
    }

    #[inline(always)]
    pub fn frames(&self) -> &[Frame] {
        &self.0.frames
    }

    #[inline(always)]
    pub fn sample_rate(&self) -> u32 {
        self.0.sample_rate
    }

    #[inline(always)]
    pub fn frame_count(&self) -> usize {
        self.0.frame_count
    }

    #[inline(always)]
    pub fn length(&self) -> f32 {
        self.0.length
    }
}