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
use crossbeam_channel::{bounded, Receiver, Sender};
use hound::{SampleFormat as WavSampleFormat, WavSpec, WavWriter};

#[derive(Debug, Clone)]
pub struct RecordingMeter {
    pub peak: f32,
}

pub struct RecordingHandle {
    stop_tx: Sender<()>,
    done_rx: Receiver<Result<PathBuf, String>>,
    meter_rx: Receiver<RecordingMeter>,
    pub path: PathBuf,
    pub start_sample: u64,
    pub target_track_id: Option<String>,
}

impl RecordingHandle {
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

pub fn start_recording(
    path: PathBuf,
    start_sample: u64,
    target_track_id: Option<String>,
    input_device: Option<String>,
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
            let result = run_recording_thread(thread_path.clone(), input_device, stop_rx, ready_tx, meter_tx);
            let _ = done_tx.send(result.map(|_| thread_path));
        })
        .map_err(|e| format!("Could not start recording thread: {e}"))?;

    ready_rx
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| "Timed out while opening the input device.".to_string())??;

    Ok(RecordingHandle { stop_tx, done_rx, meter_rx, path, start_sample, target_track_id })
}

fn run_recording_thread(
    path: PathBuf,
    input_device: Option<String>,
    stop_rx: Receiver<()>,
    ready_tx: Sender<Result<(), String>>,
    meter_tx: Sender<RecordingMeter>,
) -> Result<(), String> {
    let host = cpal::default_host();
    let device = match input_device.as_deref().filter(|name| !name.trim().is_empty()) {
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
    let spec = WavSpec {
        channels: config.channels,
        sample_rate: config.sample_rate.0,
        bits_per_sample: 32,
        sample_format: WavSampleFormat::Float,
    };
    let writer = WavWriter::create(&path, spec).map_err(|e| format!("Could not create recording WAV: {e}"))?;
    let writer = Arc::new(Mutex::new(Some(writer)));
    let stream = build_input_stream(&device, &config, sample_format, writer.clone(), meter_tx)?;

    stream.play().map_err(|e| format!("Could not start input stream: {e}"))?;
    let _ = ready_tx.send(Ok(()));
    let _ = stop_rx.recv();
    drop(stream);

    let writer = writer
        .lock()
        .map_err(|e| e.to_string())?
        .take()
        .ok_or_else(|| "Recording writer was already finalized.".to_string())?;
    writer.finalize().map_err(|e| format!("Could not finalize recording WAV: {e}"))?;
    Ok(())
}

fn build_input_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    format: SampleFormat,
    writer: Arc<Mutex<Option<WavWriter<std::io::BufWriter<std::fs::File>>>>>,
    meter_tx: Sender<RecordingMeter>,
) -> Result<cpal::Stream, String> {
    let err = |error: cpal::StreamError| {
        eprintln!("[recorder] input stream error: {error}");
    };

    match format {
        SampleFormat::F32 => {
            let writer = writer.clone();
            let meter_tx = meter_tx.clone();
            device
                .build_input_stream(
                    config,
                    move |data: &[f32], _| {
                        write_samples(data.iter().copied(), &writer);
                        emit_meter(data.iter().copied(), &meter_tx);
                    },
                    err,
                    None,
                )
                .map_err(|e| format!("Could not build f32 input stream: {e}"))
        }
        SampleFormat::I16 => {
            let writer = writer.clone();
            let meter_tx = meter_tx.clone();
            device
                .build_input_stream(
                    config,
                    move |data: &[i16], _| {
                        let converted: Vec<f32> = data.iter().map(|s| *s as f32 / i16::MAX as f32).collect();
                        write_samples(converted.iter().copied(), &writer);
                        emit_meter(converted.iter().copied(), &meter_tx);
                    },
                    err,
                    None,
                )
                .map_err(|e| format!("Could not build i16 input stream: {e}"))
        }
        SampleFormat::U16 => {
            let writer = writer.clone();
            let meter_tx = meter_tx.clone();
            device
                .build_input_stream(
                    config,
                    move |data: &[u16], _| {
                        let converted: Vec<f32> = data
                            .iter()
                            .map(|s| (*s as f32 / u16::MAX as f32) * 2.0 - 1.0)
                            .collect();
                        write_samples(converted.iter().copied(), &writer);
                        emit_meter(converted.iter().copied(), &meter_tx);
                    },
                    err,
                    None,
                )
                .map_err(|e| format!("Could not build u16 input stream: {e}"))
        }
        other => Err(format!("Unsupported input sample format: {other:?}")),
    }
}

fn emit_meter<I>(samples: I, meter_tx: &Sender<RecordingMeter>)
where
    I: IntoIterator<Item = f32>,
{
    let mut peak = 0.0_f32;
    for sample in samples {
        peak = peak.max(sample.abs());
    }
    let _ = meter_tx.try_send(RecordingMeter { peak: peak.clamp(0.0, 1.0) });
}

fn write_samples<I>(samples: I, writer: &Arc<Mutex<Option<WavWriter<std::io::BufWriter<std::fs::File>>>>>)
where
    I: IntoIterator<Item = f32>,
{
    let Ok(mut guard) = writer.lock() else {
        return;
    };
    let Some(writer) = guard.as_mut() else {
        return;
    };
    for sample in samples {
        let _ = writer.write_sample(sample.clamp(-1.0, 1.0));
    }
}
