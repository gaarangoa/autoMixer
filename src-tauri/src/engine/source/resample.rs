//! High-quality resampling via rubato. Used at import to normalize all
//! source files to the session sample rate; playback never resamples.

use rubato::{Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};

pub fn resample_interleaved(
    samples: &[f32],
    channels: u16,
    from_hz: u32,
    to_hz: u32,
) -> Result<Vec<f32>, String> {
    if from_hz == to_hz {
        return Ok(samples.to_vec());
    }
    let channels = channels.max(1) as usize;
    let frames_in = samples.len() / channels;
    if frames_in == 0 {
        return Ok(Vec::new());
    }
    let ratio = to_hz as f64 / from_hz as f64;

    let params = SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Cubic,
        oversampling_factor: 256,
        window: WindowFunction::BlackmanHarris2,
    };
    const CHUNK: usize = 2048;
    let mut resampler =
        SincFixedIn::<f32>::new(ratio, 1.0, params, CHUNK, channels).map_err(|e| e.to_string())?;

    // De-interleave into channel-planar buffers.
    let mut planar: Vec<Vec<f32>> = (0..channels).map(|_| Vec::with_capacity(frames_in)).collect();
    for frame in 0..frames_in {
        for (c, plane) in planar.iter_mut().enumerate() {
            plane.push(samples[frame * channels + c]);
        }
    }

    let mut out_planar: Vec<Vec<f32>> = (0..channels).map(|_| Vec::new()).collect();
    let mut pos = 0usize;
    while pos + CHUNK <= frames_in {
        let chunk: Vec<&[f32]> = planar.iter().map(|p| &p[pos..pos + CHUNK]).collect();
        let out = resampler.process(&chunk, None).map_err(|e| e.to_string())?;
        for (c, plane) in out.into_iter().enumerate() {
            out_planar[c].extend_from_slice(&plane);
        }
        pos += CHUNK;
    }
    // Tail: pad with zeros and run one more block, then trim.
    let remaining = frames_in - pos;
    if remaining > 0 {
        let mut padded: Vec<Vec<f32>> = planar
            .iter()
            .map(|p| {
                let mut v = Vec::with_capacity(CHUNK);
                v.extend_from_slice(&p[pos..]);
                v.resize(CHUNK, 0.0);
                v
            })
            .collect();
        let chunk: Vec<&[f32]> = padded.iter_mut().map(|v| v.as_slice()).collect();
        let out = resampler.process(&chunk, None).map_err(|e| e.to_string())?;
        let trim = (remaining as f64 * ratio).round() as usize;
        for (c, plane) in out.into_iter().enumerate() {
            let take = plane.len().min(trim);
            out_planar[c].extend_from_slice(&plane[..take]);
        }
    }

    let frames_out = out_planar.iter().map(|p| p.len()).min().unwrap_or(0);
    let mut interleaved = Vec::with_capacity(frames_out * channels);
    for frame in 0..frames_out {
        for plane in &out_planar {
            interleaved.push(plane[frame]);
        }
    }
    Ok(interleaved)
}
