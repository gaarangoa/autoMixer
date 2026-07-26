//! Simple binary cache format for normalized f32 PCM.
//!
//! Layout (little-endian):
//!   magic       u32  "AMF1"  (0x31464D41)
//!   version     u16  = 1
//!   channels    u16
//!   sample_rate u32
//!   frames      u64
//!   data        f32 * frames * channels   (interleaved)

use std::{
    fs::File,
    io::{BufReader, BufWriter, Read, Write},
    path::Path,
};

pub const MAGIC: u32 = 0x3146_4D41; // "AMF1"
pub const VERSION: u16 = 1;
pub const HEADER_BYTES: u64 = 4 + 2 + 2 + 4 + 8;

pub struct CacheHeader {
    pub channels: u16,
    pub sample_rate: u32,
    pub frames: u64,
}

pub fn write_cache(path: &Path, header: &CacheHeader, samples: &[f32]) -> Result<(), String> {
    if (samples.len() as u64) != header.frames * header.channels as u64 {
        return Err("cache: sample count mismatches header".into());
    }
    let file = File::create(path).map_err(|e| format!("create {}: {e}", path.display()))?;
    let mut w = BufWriter::new(file);
    w.write_all(&MAGIC.to_le_bytes())
        .map_err(|e| e.to_string())?;
    w.write_all(&VERSION.to_le_bytes())
        .map_err(|e| e.to_string())?;
    w.write_all(&header.channels.to_le_bytes())
        .map_err(|e| e.to_string())?;
    w.write_all(&header.sample_rate.to_le_bytes())
        .map_err(|e| e.to_string())?;
    w.write_all(&header.frames.to_le_bytes())
        .map_err(|e| e.to_string())?;
    let bytes =
        unsafe { std::slice::from_raw_parts(samples.as_ptr() as *const u8, samples.len() * 4) };
    w.write_all(bytes).map_err(|e| e.to_string())?;
    w.flush().map_err(|e| e.to_string())?;
    Ok(())
}

pub fn read_cache_all(path: &Path) -> Result<(CacheHeader, Vec<f32>), String> {
    let file = File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut r = BufReader::new(file);

    let mut buf4 = [0u8; 4];
    let mut buf2 = [0u8; 2];
    let mut buf8 = [0u8; 8];

    r.read_exact(&mut buf4).map_err(|e| e.to_string())?;
    if u32::from_le_bytes(buf4) != MAGIC {
        return Err("cache: bad magic".into());
    }
    r.read_exact(&mut buf2).map_err(|e| e.to_string())?;
    if u16::from_le_bytes(buf2) != VERSION {
        return Err("cache: unsupported version".into());
    }
    r.read_exact(&mut buf2).map_err(|e| e.to_string())?;
    let channels = u16::from_le_bytes(buf2);
    r.read_exact(&mut buf4).map_err(|e| e.to_string())?;
    let sample_rate = u32::from_le_bytes(buf4);
    r.read_exact(&mut buf8).map_err(|e| e.to_string())?;
    let frames = u64::from_le_bytes(buf8);

    let total = (frames as usize) * channels as usize;
    let mut samples = vec![0.0f32; total];
    let bytes =
        unsafe { std::slice::from_raw_parts_mut(samples.as_mut_ptr() as *mut u8, total * 4) };
    r.read_exact(bytes).map_err(|e| e.to_string())?;

    Ok((
        CacheHeader {
            channels,
            sample_rate,
            frames,
        },
        samples,
    ))
}
