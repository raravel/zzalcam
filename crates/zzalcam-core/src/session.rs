//! Append-only session file holding raw recorded frames.
//!
//! Layout (little endian):
//! ```text
//! header: "ZZCS" | version u32 | width u32 | height u32
//! record: tag u8 = 1 | timestamp_ms u64 | x u32 | y u32 | w u32 | h u32 | len u32 | lz4 block (RGBA of rect)
//! record: tag u8 = 2 | end_timestamp_ms u64
//! ```
//! A frame record stores only the rect that changed since the previous frame, except every
//! `KEYFRAME_INTERVAL`-th frame which stores the whole canvas so the editor can seek.
//! A truncated tail (crash while recording) is ignored by the reader, so sessions are recoverable.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::diff::{blit, changed_bbox, crop};
use crate::{Error, Frame, FrameSource, Rect, TimedFrame};

const MAGIC: &[u8; 4] = b"ZZCS";
const VERSION: u32 = 1;
const HEADER_LEN: u64 = 16;
const TAG_FRAME: u8 = 1;
const TAG_END: u8 = 2;
const FRAME_HEADER_LEN: usize = 1 + 8 + 16 + 4;
pub const KEYFRAME_INTERVAL: usize = 60;
/// How much recording a crash can lose at most.
const FLUSH_INTERVAL_MS: u64 = 1000;
/// Duration given to the last frame of a session that has no end record.
const FALLBACK_LAST_DURATION_MS: u64 = 100;

pub struct SessionWriter {
    out: BufWriter<File>,
    width: u32,
    height: u32,
    prev: Vec<u8>,
    frames: usize,
    last_ts: u64,
    last_flush_ts: Option<u64>,
    bytes: u64,
}

impl SessionWriter {
    pub fn create(path: impl AsRef<Path>, width: u32, height: u32) -> Result<Self, Error> {
        let mut out = BufWriter::with_capacity(1 << 20, File::create(path)?);
        out.write_all(MAGIC)?;
        out.write_all(&VERSION.to_le_bytes())?;
        out.write_all(&width.to_le_bytes())?;
        out.write_all(&height.to_le_bytes())?;
        Ok(Self {
            out,
            width,
            height,
            prev: Vec::new(),
            frames: 0,
            last_ts: 0,
            last_flush_ts: None,
            bytes: 0,
        })
    }

    pub fn frame_count(&self) -> usize {
        self.frames
    }

    /// Compressed frame bytes written so far.
    pub fn bytes_written(&self) -> u64 {
        self.bytes
    }

    /// Append a full RGBA canvas. Returns `false` if it was identical to the previous frame and skipped.
    pub fn push(&mut self, rgba: &[u8], timestamp_ms: u64) -> Result<bool, Error> {
        let expected = self.width as usize * self.height as usize * 4;
        if rgba.len() != expected {
            return Err(Error::InvalidSession(
                "frame buffer length does not match session size",
            ));
        }
        let full = Rect::new(0, 0, self.width, self.height);
        let rect = if self.frames.is_multiple_of(KEYFRAME_INTERVAL) {
            if self.frames > 0 && self.prev == rgba {
                return Ok(false);
            }
            full
        } else {
            match changed_bbox(&self.prev, rgba, self.width, self.height, 0) {
                Some(r) => r,
                None => return Ok(false),
            }
        };

        let raw = if rect == full {
            rgba.to_vec()
        } else {
            crop(rgba, self.width, rect)
        };
        let packed = lz4_flex::block::compress(&raw);
        self.out.write_all(&[TAG_FRAME])?;
        self.out.write_all(&timestamp_ms.to_le_bytes())?;
        for v in [rect.x, rect.y, rect.w, rect.h, packed.len() as u32] {
            self.out.write_all(&v.to_le_bytes())?;
        }
        self.out.write_all(&packed)?;
        self.bytes += packed.len() as u64;

        if self.prev.len() == rgba.len() {
            blit(&mut self.prev, self.width, rect, &raw);
        } else {
            self.prev = rgba.to_vec();
        }
        self.frames += 1;
        self.last_ts = timestamp_ms;
        // Release builds abort on panic without unwinding, so buffered data would be lost on a crash.
        if self
            .last_flush_ts
            .is_none_or(|t| timestamp_ms >= t + FLUSH_INTERVAL_MS)
        {
            self.out.flush()?;
            self.last_flush_ts = Some(timestamp_ms);
        }
        Ok(true)
    }

    /// Write the end marker; `end_ms` decides how long the last frame stays on screen.
    pub fn finish(mut self, end_ms: u64) -> Result<(), Error> {
        self.out.write_all(&[TAG_END])?;
        self.out
            .write_all(&end_ms.max(self.last_ts).to_le_bytes())?;
        self.out.flush()?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct IndexEntry {
    offset: u64,
    timestamp_ms: u64,
    rect: Rect,
    len: u32,
}

pub struct SessionReader {
    path: PathBuf,
    width: u32,
    height: u32,
    index: Vec<IndexEntry>,
    end_ms: u64,
}

impl SessionReader {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path)?;
        let file_len = file.metadata()?.len();
        let mut r = BufReader::with_capacity(1 << 16, file);

        let mut header = [0u8; HEADER_LEN as usize];
        r.read_exact(&mut header)
            .map_err(|_| Error::InvalidSession("header too short"))?;
        if &header[0..4] != MAGIC {
            return Err(Error::InvalidSession("bad magic"));
        }
        if u32_at(&header, 4) != VERSION {
            return Err(Error::InvalidSession("unsupported version"));
        }
        let (width, height) = (u32_at(&header, 8), u32_at(&header, 12));

        let mut index = Vec::new();
        let mut end_ms = None;
        let mut offset = HEADER_LEN;
        loop {
            let mut tag = [0u8; 1];
            if r.read_exact(&mut tag).is_err() {
                break;
            }
            match tag[0] {
                TAG_FRAME => {
                    let mut h = [0u8; FRAME_HEADER_LEN - 1];
                    if r.read_exact(&mut h).is_err() {
                        break;
                    }
                    let ts = u64::from_le_bytes(h[0..8].try_into().unwrap());
                    let rect = Rect::new(
                        u32_at(&h, 8),
                        u32_at(&h, 12),
                        u32_at(&h, 16),
                        u32_at(&h, 20),
                    );
                    let len = u32_at(&h, 24);
                    let data_offset = offset + FRAME_HEADER_LEN as u64;
                    if data_offset + len as u64 > file_len {
                        break; // truncated tail
                    }
                    let fits = |pos: u32, len: u32, max: u32| {
                        pos.checked_add(len).is_some_and(|end| end <= max)
                    };
                    if !fits(rect.x, rect.w, width) || !fits(rect.y, rect.h, height) {
                        return Err(Error::InvalidSession("frame rect out of bounds"));
                    }
                    if index.is_empty() && rect != Rect::new(0, 0, width, height) {
                        return Err(Error::InvalidSession("first frame is not a keyframe"));
                    }
                    index.push(IndexEntry {
                        offset: data_offset,
                        timestamp_ms: ts,
                        rect,
                        len,
                    });
                    r.seek_relative(len as i64)?;
                    offset = data_offset + len as u64;
                }
                TAG_END => {
                    let mut b = [0u8; 8];
                    if r.read_exact(&mut b).is_ok() {
                        end_ms = Some(u64::from_le_bytes(b));
                    }
                    break;
                }
                _ => break,
            }
        }

        let end_ms = end_ms
            .or_else(|| {
                index
                    .last()
                    .map(|e| e.timestamp_ms + FALLBACK_LAST_DURATION_MS)
            })
            .unwrap_or(0);
        Ok(Self {
            path,
            width,
            height,
            index,
            end_ms,
        })
    }

    pub fn end_ms(&self) -> u64 {
        self.end_ms
    }

    fn duration_of(&self, i: usize) -> u64 {
        let next = self
            .index
            .get(i + 1)
            .map_or(self.end_ms, |e| e.timestamp_ms);
        next.saturating_sub(self.index[i].timestamp_ms)
    }

    fn read_rect(&self, file: &mut File, e: &IndexEntry) -> Result<Vec<u8>, Error> {
        let mut packed = vec![0u8; e.len as usize];
        file.seek(SeekFrom::Start(e.offset))?;
        file.read_exact(&mut packed)?;
        let expected = e.rect.w as usize * e.rect.h as usize * 4;
        let raw = lz4_flex::block::decompress(&packed, expected)
            .map_err(|_| Error::InvalidSession("corrupt frame data"))?;
        if raw.len() != expected {
            return Err(Error::InvalidSession("frame data has wrong length"));
        }
        Ok(raw)
    }

    /// Decode a single frame by replaying from its nearest preceding keyframe.
    pub fn frame(&self, i: usize) -> Result<TimedFrame, Error> {
        if i >= self.index.len() {
            return Err(Error::InvalidSession("frame index out of range"));
        }
        let key = i - i % KEYFRAME_INTERVAL;
        let mut file = File::open(&self.path)?;
        let mut canvas = vec![0u8; self.width as usize * self.height as usize * 4];
        for e in &self.index[key..=i] {
            let raw = self.read_rect(&mut file, e)?;
            blit(&mut canvas, self.width, e.rect, &raw);
        }
        let ts = self.index[i].timestamp_ms;
        Ok(TimedFrame {
            frame: Frame::new(self.width, self.height, canvas, ts),
            duration_ms: self.duration_of(i),
        })
    }
}

impl FrameSource for SessionReader {
    fn width(&self) -> u32 {
        self.width
    }
    fn height(&self) -> u32 {
        self.height
    }
    fn frame_count(&self) -> usize {
        self.index.len()
    }
    fn frames(&self) -> Box<dyn Iterator<Item = Result<TimedFrame, Error>> + '_> {
        let mut file = match File::open(&self.path) {
            Ok(f) => f,
            Err(e) => return Box::new(std::iter::once(Err(e.into()))),
        };
        let mut canvas = vec![0u8; self.width as usize * self.height as usize * 4];
        Box::new(self.index.iter().enumerate().map(move |(i, e)| {
            let raw = self.read_rect(&mut file, e)?;
            blit(&mut canvas, self.width, e.rect, &raw);
            Ok(TimedFrame {
                frame: Frame::new(self.width, self.height, canvas.clone(), e.timestamp_ms),
                duration_ms: self.duration_of(i),
            })
        }))
    }
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("zzalcam-test-{}-{name}.zzcs", std::process::id()))
    }

    fn canvas(w: u32, h: u32, seed: u8) -> Vec<u8> {
        (0..w * h * 4)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
            .collect()
    }

    #[test]
    fn roundtrip_with_diffs_and_vfr() {
        let path = tmp("roundtrip");
        let (w, h) = (32, 20);
        let a = canvas(w, h, 0);
        let mut b = a.clone();
        b[(5 * w as usize + 7) * 4] ^= 0xff;

        let mut wr = SessionWriter::create(&path, w, h).unwrap();
        assert!(wr.push(&a, 0).unwrap());
        assert!(!wr.push(&a, 40).unwrap(), "identical frame is skipped");
        assert!(wr.push(&b, 80).unwrap());
        wr.finish(200).unwrap();

        let rd = SessionReader::open(&path).unwrap();
        assert_eq!(rd.frame_count(), 2);
        let frames: Vec<_> = rd.frames().map(Result::unwrap).collect();
        assert_eq!(frames[0].frame.rgba, a);
        assert_eq!(frames[0].duration_ms, 80);
        assert_eq!(frames[1].frame.rgba, b);
        assert_eq!(frames[1].duration_ms, 120);
        assert_eq!(rd.frame(1).unwrap().frame.rgba, b);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn keyframes_allow_seeking() {
        let path = tmp("seek");
        let (w, h) = (8, 8);
        let mut wr = SessionWriter::create(&path, w, h).unwrap();
        let frames: Vec<Vec<u8>> = (0..(KEYFRAME_INTERVAL as u8 * 2 + 5))
            .map(|s| canvas(w, h, s))
            .collect();
        for (i, f) in frames.iter().enumerate() {
            wr.push(f, i as u64 * 10).unwrap();
        }
        wr.finish(10_000).unwrap();
        let rd = SessionReader::open(&path).unwrap();
        for i in [
            0,
            1,
            KEYFRAME_INTERVAL,
            KEYFRAME_INTERVAL + 3,
            frames.len() - 1,
        ] {
            assert_eq!(rd.frame(i).unwrap().frame.rgba, frames[i], "frame {i}");
        }
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn truncated_tail_is_recoverable() {
        let path = tmp("trunc");
        let (w, h) = (16, 16);
        let mut wr = SessionWriter::create(&path, w, h).unwrap();
        wr.push(&canvas(w, h, 1), 0).unwrap();
        wr.push(&canvas(w, h, 2), 50).unwrap();
        wr.finish(100).unwrap();
        let len = std::fs::metadata(&path).unwrap().len();
        // Drop the end record and half of the last frame.
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(len - 9 - 20).unwrap();
        drop(f);
        let rd = SessionReader::open(&path).unwrap();
        assert_eq!(rd.frame_count(), 1);
        assert_eq!(
            rd.frames().next().unwrap().unwrap().duration_ms,
            FALLBACK_LAST_DURATION_MS
        );
        std::fs::remove_file(path).ok();
    }
}
