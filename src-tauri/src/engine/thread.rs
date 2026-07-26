//! Owns the cpal output stream on a dedicated thread, since `cpal::Stream`
//! is `!Send` on most platforms. UI talks to the engine via the lock-free
//! command queue and via the cross-thread control channel for lifecycle.

use std::sync::{atomic::Ordering, Arc};

use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    SampleFormat, StreamConfig,
};
use crossbeam_channel::{bounded, Receiver, Sender};
use rtrb::Consumer;

use super::{commands::EngineCommand, events::EngineEvent, mixer::Mixer, shared::EngineShared};

pub struct AudioThreadHandle {
    control_tx: Sender<ControlMessage>,
    join: Option<std::thread::JoinHandle<()>>,
}

#[derive(Debug, Clone, Copy)]
enum ControlMessage {
    Start,
    Stop,
    Shutdown,
}

pub struct AudioThreadConfig {
    pub preferred_block_size: u32,
}

pub struct AudioThreadDeps {
    pub commands: Consumer<EngineCommand>,
    pub events: Sender<EngineEvent>,
    pub shared: Arc<EngineShared>,
    pub config: AudioThreadConfig,
}

impl AudioThreadHandle {
    pub fn spawn(deps: AudioThreadDeps) -> Self {
        let (control_tx, control_rx) = bounded::<ControlMessage>(8);
        let join = std::thread::Builder::new()
            .name("automixer-audio".into())
            .spawn(move || run_audio_thread(deps, control_rx))
            .expect("failed to spawn audio thread");
        Self {
            control_tx,
            join: Some(join),
        }
    }

    pub fn start(&self) {
        let _ = self.control_tx.send(ControlMessage::Start);
    }

    pub fn stop(&self) {
        let _ = self.control_tx.send(ControlMessage::Stop);
    }
}

impl Drop for AudioThreadHandle {
    fn drop(&mut self) {
        let _ = self.control_tx.send(ControlMessage::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn run_audio_thread(deps: AudioThreadDeps, control_rx: Receiver<ControlMessage>) {
    let AudioThreadDeps {
        commands,
        events,
        shared,
        config,
    } = deps;

    let host = cpal::default_host();
    let Some(device) = host.default_output_device() else {
        eprintln!("[engine] no default output device");
        return;
    };
    let Ok(default_config) = device.default_output_config() else {
        eprintln!("[engine] could not query default output config");
        return;
    };

    let sample_format = default_config.sample_format();
    let mut stream_config: StreamConfig = default_config.config();
    if config.preferred_block_size > 0 {
        stream_config.buffer_size = cpal::BufferSize::Fixed(config.preferred_block_size);
    }

    let sample_rate = stream_config.sample_rate.0 as f32;
    let channels = stream_config.channels;
    let mixer = Mixer::new(
        sample_rate,
        channels,
        commands,
        events.clone(),
        shared.clone(),
    );

    let stream = match build_stream(&device, &stream_config, sample_format, mixer) {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!("[engine] failed to build output stream: {error}");
            return;
        }
    };

    let mut running = match stream.play() {
        Ok(()) => {
            shared.running.store(false, Ordering::Relaxed);
            true
        }
        Err(error) => {
            eprintln!("[engine] initial stream.play failed: {error}");
            false
        }
    };
    loop {
        match control_rx.recv() {
            Ok(ControlMessage::Start) => {
                if !running {
                    if let Err(error) = stream.play() {
                        eprintln!("[engine] stream.play failed: {error}");
                    } else {
                        running = true;
                        shared.running.store(true, Ordering::Relaxed);
                    }
                }
            }
            Ok(ControlMessage::Stop) => {
                shared.running.store(false, Ordering::Relaxed);
            }
            Ok(ControlMessage::Shutdown) | Err(_) => {
                let _ = stream.pause();
                shared.running.store(false, Ordering::Relaxed);
                break;
            }
        }
    }
    let _ = events.send(EngineEvent::Stopped);
}

fn build_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    format: SampleFormat,
    mut mixer: Mixer,
) -> Result<cpal::Stream, cpal::BuildStreamError> {
    let err = |error: cpal::StreamError| {
        eprintln!("[engine] stream error: {error}");
    };

    match format {
        SampleFormat::F32 => device.build_output_stream(
            config,
            move |data: &mut [f32], _| {
                mixer.render(data);
            },
            err,
            None,
        ),
        SampleFormat::I16 => {
            let channels = config.channels as usize;
            device.build_output_stream(
                config,
                move |data: &mut [i16], _| {
                    let mut scratch = [0.0_f32; 4096];
                    let mut written = 0;
                    while written < data.len() {
                        let frames_remaining = (data.len() - written) / channels;
                        let chunk_frames = frames_remaining.min(scratch.len() / channels);
                        if chunk_frames == 0 {
                            break;
                        }
                        let len_samples = chunk_frames * channels;
                        mixer.render(&mut scratch[..len_samples]);
                        for (out, sample) in data[written..written + len_samples]
                            .iter_mut()
                            .zip(scratch[..len_samples].iter())
                        {
                            let clipped = sample.clamp(-1.0, 1.0);
                            *out = (clipped * i16::MAX as f32) as i16;
                        }
                        written += len_samples;
                    }
                },
                err,
                None,
            )
        }
        SampleFormat::U16 => {
            let channels = config.channels as usize;
            device.build_output_stream(
                config,
                move |data: &mut [u16], _| {
                    let mut scratch = [0.0_f32; 4096];
                    let mut written = 0;
                    while written < data.len() {
                        let frames_remaining = (data.len() - written) / channels;
                        let chunk_frames = frames_remaining.min(scratch.len() / channels);
                        if chunk_frames == 0 {
                            break;
                        }
                        let len_samples = chunk_frames * channels;
                        mixer.render(&mut scratch[..len_samples]);
                        for (out, sample) in data[written..written + len_samples]
                            .iter_mut()
                            .zip(scratch[..len_samples].iter())
                        {
                            let clipped = sample.clamp(-1.0, 1.0);
                            let mid = i16::MAX as f32;
                            *out = ((clipped * mid) + mid) as u16;
                        }
                        written += len_samples;
                    }
                },
                err,
                None,
            )
        }
        other => {
            eprintln!("[engine] unsupported sample format {other:?}");
            Err(cpal::BuildStreamError::StreamConfigNotSupported)
        }
    }
}
