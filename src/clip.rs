use crate::Frame;
use anyhow::{anyhow, bail, Result};
use std::{io::Cursor, sync::Arc};
use symphonia::core::{
    audio::{AudioBuffer, AudioBufferRef, Signal},
    io::MediaSourceStream,
};

struct ClipInner {
    frames: Vec<Frame>,
    sample_rate: u32,
}

pub struct AudioClip(Arc<ClipInner>);

impl Clone for AudioClip {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl AudioClip {
    pub fn from_raw(frames: Vec<Frame>, sample_rate: u32) -> Self {
        Self(Arc::new(ClipInner {
            frames,
            sample_rate,
        }))
    }
    
    pub fn decode(data: Vec<u8>) -> Result<(Vec<Frame>, u32)> {
        let mut frames = Vec::with_capacity(data.len() / 100);
    
        #[inline(always)]
        fn load_frames_from_buffer(
            frames: &mut Vec<Frame>,
            buffer: &AudioBuffer<f32>,
        ) {
            let channels = buffer.spec().channels.count();
            if channels == 1 {
                let chan = buffer.chan(0);
                frames.reserve(chan.len());
                frames.extend(chan.iter().map(|&sample| Frame(sample, sample)));
            } else {
                let iter = buffer.chan(0).iter().zip(buffer.chan(1));
                frames.reserve(iter.len());
                frames.extend(iter.map(|(&left, &right)| Frame(left, right)));
            }
        }
    
        #[inline(always)]
        fn load_frames_from_buffer_ref(
            frames: &mut Vec<Frame>,
            buffer: &AudioBufferRef,
        ) -> Result<()> {
            macro_rules! conv {
                ($buf:ident) => {{
                    let mut dest = AudioBuffer::new(buffer.capacity() as u64, buffer.spec().clone());
                    $buf.convert(&mut dest);
                    load_frames_from_buffer(frames, &dest);
                }};
            }
            use AudioBufferRef::*;
            match buffer {
                F32(buf) => load_frames_from_buffer(frames, buf),
                U8(buf) => conv!(buf),
                U16(buf) => conv!(buf),
                U24(buf) => conv!(buf),
                U32(buf) => conv!(buf),
                S8(buf) => conv!(buf),
                S16(buf) => conv!(buf),
                S24(buf) => conv!(buf),
                S32(buf) => conv!(buf),
                F64(buf) => conv!(buf),
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
    
        let codec_params = &format_reader
            .default_track()
            .ok_or_else(|| anyhow!("default track not found"))?
            .codec_params;
        let sample_rate = codec_params
            .sample_rate
            .ok_or_else(|| anyhow!("unknown sample rate"))?;
    
        let mut decoder = codecs.make(codec_params, &Default::default())?;
        loop {
            match format_reader.next_packet() {
                Ok(packet) => {
                    let buffer = match decoder.decode(&packet) {
                        Ok(buffer) => buffer,
                        Err(symphonia::core::errors::Error::DecodeError(msg))
                            if msg.contains("invalid main_data offset") =>
                        {
                            continue;
                        }
                        Err(err) => return Err(err.into()),
                    };
                    load_frames_from_buffer_ref(&mut frames, &buffer)?;
                }
                Err(error) => match error {
                    symphonia::core::errors::Error::IoError(ref io_err)
                        if io_err.kind() == std::io::ErrorKind::UnexpectedEof =>
                    {
                        break;
                    }
                    _ => bail!(error),
                },
            }
        }
        Ok((frames, sample_rate))
    }
    
    #[inline]
    pub fn new(data: Vec<u8>) -> Result<Self> {
        let (frames, sample_rate) = Self::decode(data)?;
        Ok(Self::from_raw(frames, sample_rate))
    }
    
    pub fn sample(&self, position: f32) -> Option<Frame> {
        let pos = position * self.0.sample_rate as f32;
        let index = pos as usize;
        self.0.frames.get(index).map(|frame| {
            let next_frame = self.0.frames.get(index + 1).unwrap_or(frame);
            frame.interpolate(next_frame, pos - index as f32)
        })
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
        self.0.frames.len()
    }
    
    pub fn length(&self) -> f32 {
        self.frame_count() as f32 / self.sample_rate() as f32
    }
}
