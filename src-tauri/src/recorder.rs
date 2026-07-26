use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    SampleFormat, StreamConfig,
};
use crossbeam_channel::{bounded, Receiver, Sender, TryRecvError};
use hound::{SampleFormat as WavSampleFormat, WavSpec, WavWriter};

#[derive(Debug, Clone)]
pub struct RecordingMeter {
    pub peak: f32,
    /// Peak per input channel for the buffer. Empty when channel count is unknown.
    pub channel_peaks: Vec<f32>,
}

pub struct RecordingHandle {
    stop_tx: Sender<()>,
    ready_rx: Receiver<Result<(), String>>,
    done_rx: Receiver<Result<PathBuf, String>>,
    meter_rx: Receiver<RecordingMeter>,
    pub path: PathBuf,
    pub start_sample: u64,
    pub target_track_id: Option<String>,
}

pub struct InputMonitorHandle {
    stop_tx: Sender<()>,
    ready_rx: Receiver<Result<(), String>>,
    done_rx: Receiver<()>,
    meter_rx: Receiver<RecordingMeter>,
}

impl RecordingHandle {
    pub fn wait_until_ready(&self, timeout: Duration) -> Result<(), String> {
        self.ready_rx
            .recv_timeout(timeout)
            .map_err(|_| "Timed out while opening the recording input.".to_string())?
    }

    pub fn stop(self) -> Result<PathBuf, String> {
        let _ = self.stop_tx.send(());
        self.done_rx
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| "Timed out while stopping recording.".to_string())?
    }

    pub fn drain_meters(&self) -> Vec<RecordingMeter> {
        let mut meters = Vec::new();
        while let Ok(meter) = self.meter_rx.try_recv() {
            meters.push(meter);
        }
        meters
    }

    pub fn startup_error(&self) -> Option<String> {
        match self.ready_rx.try_recv() {
            Ok(Ok(())) | Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => None,
            Ok(Err(error)) => Some(error),
        }
    }
}

impl InputMonitorHandle {
    pub fn wait_until_ready(&self, timeout: Duration) -> Result<(), String> {
        self.ready_rx
            .recv_timeout(timeout)
            .map_err(|_| "Timed out while opening the input monitor.".to_string())?
    }

    pub fn stop(self) -> Result<(), String> {
        let _ = self.stop_tx.send(());
        self.done_rx
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| "Timed out while stopping input monitor.".to_string())?;
        Ok(())
    }

    pub fn drain_meters(&self) -> Vec<RecordingMeter> {
        let mut meters = Vec::new();
        while let Ok(meter) = self.meter_rx.try_recv() {
            meters.push(meter);
        }
        meters
    }

    pub fn startup_error(&self) -> Option<String> {
        match self.ready_rx.try_recv() {
            Ok(Ok(())) | Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => None,
            Ok(Err(error)) => Some(error),
        }
    }
}

pub fn input_devices() -> Result<Vec<String>, String> {
    let host = cpal::default_host();
    let devices = host.input_devices().map_err(|e| e.to_string())?;
    let mut names = Vec::new();
    for device in devices {
        if let Ok(name) = device.name() {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    Ok(names)
}

/// Number of input channels exposed by an audio interface (or default input).
pub fn input_device_channel_count(input_device: Option<String>) -> Result<u32, String> {
    let host = cpal::default_host();
    let device = match input_device
        .as_deref()
        .filter(|name| !name.trim().is_empty())
    {
        Some(name) => host
            .input_devices()
            .map_err(|e| e.to_string())?
            .find(|device| device.name().map(|n| n == name).unwrap_or(false))
            .ok_or_else(|| format!("Input device `{name}` is not available."))?,
        None => host
            .default_input_device()
            .ok_or_else(|| "No default input device is available.".to_string())?,
    };
    let config = device
        .default_input_config()
        .map_err(|e| format!("Could not query default input config: {e}"))?;
    Ok(config.channels() as u32)
}

pub fn start_recording(
    path: PathBuf,
    start_sample: u64,
    target_track_id: Option<String>,
    input_device: Option<String>,
    target_channels: u16,
    gain_factor: f32,
    selected_channels: Option<Vec<u16>>,
) -> Result<RecordingHandle, String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let (stop_tx, stop_rx) = bounded::<()>(1);
    let (ready_tx, ready_rx) = bounded::<Result<(), String>>(1);
    let (done_tx, done_rx) = bounded::<Result<PathBuf, String>>(1);
    let (meter_tx, meter_rx) = bounded::<RecordingMeter>(256);
    let thread_path = path.clone();

    std::thread::Builder::new()
        .name("automixer-recorder".into())
        .spawn(move || {
            let result = run_recording_thread(
                thread_path.clone(),
                input_device,
                target_channels,
                gain_factor,
                selected_channels,
                stop_rx,
                ready_tx.clone(),
                meter_tx,
            );
            if let Err(error) = &result {
                let _ = ready_tx.try_send(Err(error.clone()));
            }
            let _ = done_tx.send(result.map(|_| thread_path));
        })
        .map_err(|e| format!("Could not start recording thread: {e}"))?;

    Ok(RecordingHandle {
        stop_tx,
        ready_rx,
        done_rx,
        meter_rx,
        path,
        start_sample,
        target_track_id,
    })
}

pub fn start_input_monitor(input_device: Option<String>) -> Result<InputMonitorHandle, String> {
    let (stop_tx, stop_rx) = bounded::<()>(1);
    let (ready_tx, ready_rx) = bounded::<Result<(), String>>(1);
    let (done_tx, done_rx) = bounded::<()>(1);
    let (meter_tx, meter_rx) = bounded::<RecordingMeter>(256);

    std::thread::Builder::new()
        .name("automixer-input-monitor".into())
        .spawn(move || {
            let result = run_monitor_thread(input_device, stop_rx, ready_tx.clone(), meter_tx);
            if let Err(error) = result {
                let _ = ready_tx.try_send(Err(error));
            }
            let _ = done_tx.send(());
        })
        .map_err(|e| format!("Could not start input monitor thread: {e}"))?;

    Ok(InputMonitorHandle {
        stop_tx,
        ready_rx,
        done_rx,
        meter_rx,
    })
}

fn run_recording_thread(
    path: PathBuf,
    input_device: Option<String>,
    target_channels: u16,
    gain_factor: f32,
    selected_channels: Option<Vec<u16>>,
    stop_rx: Receiver<()>,
    ready_tx: Sender<Result<(), String>>,
    meter_tx: Sender<RecordingMeter>,
) -> Result<(), String> {
    let host = cpal::default_host();
    let device = match input_device
        .as_deref()
        .filter(|name| !name.trim().is_empty())
    {
        Some(name) => host
            .input_devices()
            .map_err(|e| e.to_string())?
            .find(|device| device.name().map(|n| n == name).unwrap_or(false))
            .ok_or_else(|| format!("Input device `{name}` is not available."))?,
        None => host
            .default_input_device()
            .ok_or_else(|| "No default input device is available.".to_string())?,
    };
    let default_config = device
        .default_input_config()
        .map_err(|e| format!("Could not query default input config: {e}"))?;
    let sample_format = default_config.sample_format();
    let config: StreamConfig = default_config.config();
    let target_channels = target_channels.max(1).min(2);
    let spec = WavSpec {
        channels: target_channels,
        sample_rate: config.sample_rate.0,
        bits_per_sample: 32,
        sample_format: WavSampleFormat::Float,
    };
    let writer = WavWriter::create(&path, spec)
        .map_err(|e| format!("Could not create recording WAV: {e}"))?;
    let writer = Arc::new(Mutex::new(Some(writer)));
    let stream = build_input_stream(
        &device,
        &config,
        sample_format,
        target_channels,
        gain_factor,
        selected_channels,
        writer.clone(),
        meter_tx,
    )?;

    stream
        .play()
        .map_err(|e| format!("Could not start input stream: {e}"))?;
    let _ = ready_tx.send(Ok(()));
    let _ = stop_rx.recv();
    drop(stream);

    let writer = writer
        .lock()
        .map_err(|e| e.to_string())?
        .take()
        .ok_or_else(|| "Recording writer was already finalized.".to_string())?;
    writer
        .finalize()
        .map_err(|e| format!("Could not finalize recording WAV: {e}"))?;
    Ok(())
}

fn run_monitor_thread(
    input_device: Option<String>,
    stop_rx: Receiver<()>,
    ready_tx: Sender<Result<(), String>>,
    meter_tx: Sender<RecordingMeter>,
) -> Result<(), String> {
    let host = cpal::default_host();
    let device = match input_device
        .as_deref()
        .filter(|name| !name.trim().is_empty())
    {
        Some(name) => host
            .input_devices()
            .map_err(|e| e.to_string())?
            .find(|device| device.name().map(|n| n == name).unwrap_or(false))
            .ok_or_else(|| format!("Input device `{name}` is not available."))?,
        None => host
            .default_input_device()
            .ok_or_else(|| "No default input device is available.".to_string())?,
    };
    let default_config = device
        .default_input_config()
        .map_err(|e| format!("Could not query default input config: {e}"))?;
    let sample_format = default_config.sample_format();
    let config: StreamConfig = default_config.config();
    let stream = build_meter_stream(&device, &config, sample_format, meter_tx)?;

    stream
        .play()
        .map_err(|e| format!("Could not start input monitor: {e}"))?;
    let _ = ready_tx.send(Ok(()));
    let _ = stop_rx.recv();
    drop(stream);
    Ok(())
}

fn build_input_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    format: SampleFormat,
    target_channels: u16,
    gain_factor: f32,
    selected_channels: Option<Vec<u16>>,
    writer: Arc<Mutex<Option<WavWriter<std::io::BufWriter<std::fs::File>>>>>,
    meter_tx: Sender<RecordingMeter>,
) -> Result<cpal::Stream, String> {
    let err = |error: cpal::StreamError| {
        eprintln!("[recorder] input stream error: {error}");
    };
    let channel_count = config.channels as usize;
    let target = (target_channels as usize).max(1).min(2);
    // Default: take the first N channels (legacy behaviour). When the user picked
    // specific channels in the inspector, use those instead.
    let selected: Vec<usize> = match selected_channels {
        Some(v) if !v.is_empty() => v
            .into_iter()
            .map(|c| (c as usize).min(channel_count.saturating_sub(1)))
            .collect(),
        _ => (0..channel_count).take(target).collect(),
    };

    match format {
        SampleFormat::F32 => {
            let writer = writer.clone();
            let meter_tx = meter_tx.clone();
            let selected = selected.clone();
            let channels = channel_count;
            device
                .build_input_stream(
                    config,
                    move |data: &[f32], _| {
                        write_recording_samples(
                            data,
                            channels,
                            target,
                            gain_factor,
                            &selected,
                            &writer,
                        );
                        emit_meter(data.iter().copied(), channels, &meter_tx);
                    },
                    err,
                    None,
                )
                .map_err(|e| format!("Could not build f32 input stream: {e}"))
        }
        SampleFormat::I16 => {
            let writer = writer.clone();
            let meter_tx = meter_tx.clone();
            let selected = selected.clone();
            let channels = channel_count;
            device
                .build_input_stream(
                    config,
                    move |data: &[i16], _| {
                        let converted: Vec<f32> =
                            data.iter().map(|s| *s as f32 / i16::MAX as f32).collect();
                        write_recording_samples(
                            &converted,
                            channels,
                            target,
                            gain_factor,
                            &selected,
                            &writer,
                        );
                        emit_meter(converted.iter().copied(), channels, &meter_tx);
                    },
                    err,
                    None,
                )
                .map_err(|e| format!("Could not build i16 input stream: {e}"))
        }
        SampleFormat::U16 => {
            let writer = writer.clone();
            let meter_tx = meter_tx.clone();
            let selected = selected.clone();
            let channels = channel_count;
            device
                .build_input_stream(
                    config,
                    move |data: &[u16], _| {
                        let converted: Vec<f32> = data
                            .iter()
                            .map(|s| (*s as f32 / u16::MAX as f32) * 2.0 - 1.0)
                            .collect();
                        write_recording_samples(
                            &converted,
                            channels,
                            target,
                            gain_factor,
                            &selected,
                            &writer,
                        );
                        emit_meter(converted.iter().copied(), channels, &meter_tx);
                    },
                    err,
                    None,
                )
                .map_err(|e| format!("Could not build u16 input stream: {e}"))
        }
        other => Err(format!("Unsupported input sample format: {other:?}")),
    }
}

fn build_meter_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    format: SampleFormat,
    meter_tx: Sender<RecordingMeter>,
) -> Result<cpal::Stream, String> {
    let err = |error: cpal::StreamError| {
        eprintln!("[recorder] input monitor stream error: {error}");
    };

    let channels = config.channels as usize;
    match format {
        SampleFormat::F32 => device
            .build_input_stream(
                config,
                move |data: &[f32], _| emit_meter(data.iter().copied(), channels, &meter_tx),
                err,
                None,
            )
            .map_err(|e| format!("Could not build f32 input monitor: {e}")),
        SampleFormat::I16 => device
            .build_input_stream(
                config,
                move |data: &[i16], _| {
                    let converted = data.iter().map(|s| *s as f32 / i16::MAX as f32);
                    emit_meter(converted, channels, &meter_tx);
                },
                err,
                None,
            )
            .map_err(|e| format!("Could not build i16 input monitor: {e}")),
        SampleFormat::U16 => device
            .build_input_stream(
                config,
                move |data: &[u16], _| {
                    let converted = data
                        .iter()
                        .map(|s| (*s as f32 / u16::MAX as f32) * 2.0 - 1.0);
                    emit_meter(converted, channels, &meter_tx);
                },
                err,
                None,
            )
            .map_err(|e| format!("Could not build u16 input monitor: {e}")),
        other => Err(format!(
            "Unsupported input monitor sample format: {other:?}"
        )),
    }
}

fn emit_meter<I>(samples: I, channels: usize, meter_tx: &Sender<RecordingMeter>)
where
    I: IntoIterator<Item = f32>,
{
    let channels = channels.max(1);
    let mut channel_peaks: Vec<f32> = vec![0.0; channels];
    let mut peak = 0.0_f32;
    let mut idx: usize = 0;
    for sample in samples {
        let abs = sample.abs();
        peak = peak.max(abs);
        let ch = idx % channels;
        if abs > channel_peaks[ch] {
            channel_peaks[ch] = abs;
        }
        idx += 1;
    }
    for v in &mut channel_peaks {
        *v = v.clamp(0.0, 1.0);
    }
    let _ = meter_tx.try_send(RecordingMeter {
        peak: peak.clamp(0.0, 1.0),
        channel_peaks,
    });
}

fn write_recording_samples(
    samples: &[f32],
    input_channels: usize,
    target_channels: usize,
    gain: f32,
    selected: &[usize],
    writer: &Arc<Mutex<Option<WavWriter<std::io::BufWriter<std::fs::File>>>>>,
) {
    let Ok(mut guard) = writer.lock() else {
        return;
    };
    let Some(writer) = guard.as_mut() else {
        return;
    };
    let input_channels = input_channels.max(1);
    let target_channels = target_channels.max(1).min(2);
    // Map each output channel to a specific input-channel index. Falls back to 0/1 so
    // we never panic on a misconfigured slice.
    let ch0 = selected
        .first()
        .copied()
        .unwrap_or(0)
        .min(input_channels.saturating_sub(1));
    let ch1 = selected
        .get(1)
        .copied()
        .unwrap_or(ch0)
        .min(input_channels.saturating_sub(1));
    for frame in samples.chunks(input_channels) {
        if frame.is_empty() {
            continue;
        }
        match target_channels {
            1 => {
                // Mono target: pick the single user-selected input channel (defaults to 0).
                let sample = frame.get(ch0).copied().unwrap_or(0.0);
                let mono = (sample * gain).clamp(-1.0, 1.0);
                let _ = writer.write_sample(mono);
            }
            _ => {
                // Stereo target: take the user-selected L and R input channels (defaults to 0/1).
                let l = (frame.get(ch0).copied().unwrap_or(0.0) * gain).clamp(-1.0, 1.0);
                let r = (frame.get(ch1).copied().unwrap_or(0.0) * gain).clamp(-1.0, 1.0);
                let _ = writer.write_sample(l);
                let _ = writer.write_sample(r);
            }
        }
    }
}
