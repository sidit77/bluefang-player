use std::array::from_fn;
use std::iter::zip;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::Arc;

use bluefang::avdtp::capabilities::{Capability, MediaCodecCapability};
use bluefang::avdtp::StreamHandler;
use bytes::Bytes;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{default_host, SampleFormat, Stream, StreamConfig};
use portable_atomic::AtomicF32;
use ringbuf::consumer::Consumer;
use ringbuf::producer::Producer;
use ringbuf::traits::Split;
use ringbuf::{HeapProd, HeapRb};
use rubato::{FftFixedIn, Resampler};
use sbc_rs::{Decoder, OutputFormat, Error};
use tracing::{error, trace, warn};

pub struct SbcStreamHandler {
    audio_session: AudioSession,
    resampler: FftFixedIn<f32>,
    decoder: Box<Decoder>,
    volume: Arc<AtomicF32>,
    decode_buffer_l: Vec<i16>,
    decode_buffer_r: Vec<i16>,
    input_buffers: [Vec<f32>; 2],
    output_buffers: [Vec<f32>; 2],
    interleave_buffer: Vec<i16>
}

impl SbcStreamHandler {
    pub fn new(volume: Arc<AtomicF32>, capabilities: &[Capability]) -> Self {
        let (source_frequency, input_size) = Self::parse_capabilities(capabilities).expect("Invalid capabilities");

        let audio_session = AudioSession::new();

        let resampler = FftFixedIn::<f32>::new(
            source_frequency as usize,
            audio_session.config().sample_rate.0 as usize,
            input_size as usize,
            1,
            2
        )
        .unwrap();

        Self {
            decoder: Box::new(Decoder::default()),
            volume,
            decode_buffer_l: vec![0i16; Decoder::MAX_SAMPLES],
            decode_buffer_r: vec![0i16; Decoder::MAX_SAMPLES],
            input_buffers: from_fn(|_| vec![0f32; resampler.input_frames_max()]),
            output_buffers: from_fn(|_| vec![0f32; resampler.output_frames_max()]),
            interleave_buffer: Vec::with_capacity(2 * resampler.output_frames_max()),
            audio_session,
            resampler
        }
    }

    fn parse_capabilities(capabilities: &[Capability]) -> Option<(u32, u32)> {
        let sbc_info = capabilities.iter().find_map(|cap| match cap {
            Capability::MediaCodec(MediaCodecCapability::Sbc(info)) => Some(info),
            _ => None
        })?;
        let frequency = sbc_info.sampling_frequencies.as_value()?;

        let subbands = sbc_info.subbands.as_value()?;

        let block_length = sbc_info.block_lengths.as_value()?;

        Some((frequency, subbands * block_length))
    }

    fn process_frames(&mut self, data: &[u8], bpp: usize) {
        //println!("buffer: {}", self.audio_session.writer().occupied_len());

        let mut index = 0;
        while index < data.len() {
            match self.decoder.decode(&data[index..], OutputFormat::Planar(&mut self.decode_buffer_l, &mut self.decode_buffer_r)) {
                Ok(status) => {
                    debug_assert_eq!(status.bytes_read, bpp, "frame size mismatch");
                    index += status.bytes_read;
                    self.input_buffers[0].clear();
                    self.input_buffers[0].extend(self.decode_buffer_l.iter().take(status.samples_written).map(|s| *s as f32));
                    let right = if status.channels == 1 { &self.decode_buffer_l } else { &self.decode_buffer_r };
                    self.input_buffers[1].clear();
                    self.input_buffers[1].extend(right.iter().take(status.samples_written).map(|s| *s as f32));
                }
                Err(Error::NotEnoughData { .. }) => {
                    warn!("Not enough data to decode frame. This should not happen; was the frame incomplete?");
                    break;
                },
                Err(Error::OutputBufferTooSmall { .. }) => unreachable!(),
                Err(Error::BadData(reason)) => {
                    error!("Failed to decode frame: {:?}", reason);
                    index += bpp;
                }
            }

            let (_, len) = self
                .resampler
                .process_into_buffer(&mut self.input_buffers, &mut self.output_buffers, None)
                .unwrap();

            self.interleave_buffer.clear();
            let volume = self.volume.load(SeqCst).powi(2);
            for (&l, &r) in zip(&self.output_buffers[0], &self.output_buffers[1]).take(len) {
                self.interleave_buffer.push((l * volume) as i16);
                self.interleave_buffer.push((r * volume) as i16);
            }
            self.audio_session
                .writer()
                .push_slice(&self.interleave_buffer);
        }

    }
}

impl StreamHandler for SbcStreamHandler {
    fn on_play(&mut self) {
        self.audio_session.play();
    }

    fn on_stop(&mut self) {
        self.audio_session.stop();
    }

    fn on_data(&mut self, data: Bytes) {
        //TODO actually parse the header to make sure the packets are not fragmented
        assert_eq!(data.as_ref()[0] & 0x80, 0, "fragmented packets are not supported");
        let packets = (data.as_ref()[0] & 0x0F) as usize;
        debug_assert!((data.len() - 1) % packets == 0, "invalid number of packet");
        let bbp = (data.len() - 1) / packets;
        self.process_frames(&data.as_ref()[1..], bbp);
    }
}

pub struct AudioSession {
    stream: Stream,
    config: StreamConfig,
    buffer: HeapProd<i16>
}

impl AudioSession {
    pub fn new() -> Self {
        let host = default_host();
        let device = host
            .default_output_device()
            .expect("failed to find output device");

        let config = device
            .supported_output_configs()
            .unwrap()
            .inspect(|config| trace!("supported output config: {:?}", config))
            .find(|config| config.sample_format() == SampleFormat::I16 && config.channels() == 2)
            .expect("failed to find output config")
            .with_max_sample_rate()
            .config();
        trace!("selected output config: {:?}", config);

        let max_buffer_size = (config.sample_rate.0 * config.channels as u32) as usize;
        let buffer: Arc<HeapRb<i16>> = Arc::new(HeapRb::new(max_buffer_size));
        let (buffer, mut consumer) = buffer.split();

        let stream = device
            .build_output_stream(
                &config,
                move |data: &mut [i16], _info| {
                    let len = consumer.pop_slice(data);
                    //data[..len].iter_mut().for_each(|d| *d *=  8);
                    data[len..].fill(0);
                },
                move |err| {
                    error!("an error occurred on the output stream: {}", err);
                },
                None
            )
            .unwrap();

        Self { stream, config, buffer }
    }

    pub fn play(&self) {
        self.stream.play().unwrap();
    }

    pub fn stop(&self) {
        self.stream.pause().unwrap();
    }

    pub fn writer(&mut self) -> &mut HeapProd<i16> {
        &mut self.buffer
    }

    pub fn config(&self) -> &StreamConfig {
        &self.config
    }
}
