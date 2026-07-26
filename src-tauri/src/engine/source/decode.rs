//! Multi-format audio decode via symphonia.
//! Returns interleaved f32 PCM at the source's native sample rate.

use std::{fs::File, path::Path};

use symphonia::core::{
    audio::{AudioBufferRef, Signal},
    codecs::DecoderOptions,
    errors::Error as SymphoniaError,
    formats::FormatOptions,
    io::MediaSourceStream,
    meta::MetadataOptions,
    probe::Hint,
};

pub struct DecodedAudio {
    pub samples: Vec<f32>,
    pub channels: u16,
    pub sample_rate: u32,
}

pub fn decode_file(path: &Path) -> Result<DecodedAudio, String> {
    let file = File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probe = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("probe: {e}"))?;
    let mut format = probe.format;

    let track = format
        .default_track()
        .ok_or_else(|| "no default audio track".to_string())?;
    let codec_params = track.codec_params.clone();
    let track_id = track.id;

    let sample_rate = codec_params
        .sample_rate
        .ok_or_else(|| "missing sample rate".to_string())?;
    let channels = codec_params
        .channels
        .map(|c| c.count() as u16)
        .unwrap_or(2)
        .max(1);

    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .map_err(|e| format!("make decoder: {e}"))?;

    let mut samples: Vec<f32> = Vec::new();

    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(ref io))
                if io.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break
            }
            Err(SymphoniaError::ResetRequired) => {
                let _ = decoder.reset();
                continue;
            }
            Err(e) => return Err(format!("read packet: {e}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => append_buffer(&buf, &mut samples, channels as usize),
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(SymphoniaError::IoError(ref io))
                if io.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break
            }
            Err(SymphoniaError::ResetRequired) => {
                let _ = decoder.reset();
                continue;
            }
            Err(e) => return Err(format!("decode: {e}")),
        }
    }

    Ok(DecodedAudio {
        samples,
        channels,
        sample_rate,
    })
}

fn append_buffer(buf: &AudioBufferRef<'_>, out: &mut Vec<f32>, channels: usize) {
    match buf {
        AudioBufferRef::F32(b) => interleave(b.frames(), channels, out, |c, f| b.chan(c)[f]),
        AudioBufferRef::F64(b) => interleave(b.frames(), channels, out, |c, f| b.chan(c)[f] as f32),
        AudioBufferRef::S8(b) => interleave(b.frames(), channels, out, |c, f| {
            b.chan(c)[f] as f32 / i8::MAX as f32
        }),
        AudioBufferRef::S16(b) => interleave(b.frames(), channels, out, |c, f| {
            b.chan(c)[f] as f32 / i16::MAX as f32
        }),
        AudioBufferRef::S24(b) => interleave(b.frames(), channels, out, |c, f| {
            b.chan(c)[f].inner() as f32 / 8_388_607.0
        }),
        AudioBufferRef::S32(b) => interleave(b.frames(), channels, out, |c, f| {
            b.chan(c)[f] as f32 / i32::MAX as f32
        }),
        AudioBufferRef::U8(b) => interleave(b.frames(), channels, out, |c, f| {
            (b.chan(c)[f] as f32 - 128.0) / 128.0
        }),
        AudioBufferRef::U16(b) => interleave(b.frames(), channels, out, |c, f| {
            (b.chan(c)[f] as f32 - 32_768.0) / 32_768.0
        }),
        AudioBufferRef::U24(b) => interleave(b.frames(), channels, out, |c, f| {
            (b.chan(c)[f].inner() as f32 - 8_388_608.0) / 8_388_608.0
        }),
        AudioBufferRef::U32(b) => interleave(b.frames(), channels, out, |c, f| {
            (b.chan(c)[f] as f64 - 2_147_483_648.0) as f32 / 2_147_483_648.0
        }),
    }
}

fn interleave<F: Fn(usize, usize) -> f32>(
    frames: usize,
    channels: usize,
    out: &mut Vec<f32>,
    sample: F,
) {
    out.reserve(frames * channels);
    for f in 0..frames {
        for c in 0..channels {
            out.push(sample(c, f).clamp(-1.0, 1.0));
        }
    }
}
