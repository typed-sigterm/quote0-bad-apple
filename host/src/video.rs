//! ffmpeg-based video frame source.
//!
//! Spawns an `ffmpeg` child process that decodes the video, samples it at the
//! requested fps, letterboxes it onto the panel geometry (optionally rotated)
//! and streams raw 8-bit grayscale frames to stdout. We read fixed-size frames
//! from the pipe; pipe backpressure naturally throttles ffmpeg.

use std::io::{self, Read};
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};

use crate::dither::{GRAY_BYTES, HEIGHT, WIDTH};

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Rotate {
    None,
    /// 90 degrees clockwise (landscape video on a portrait panel held sideways).
    Cw,
    Ccw,
}

pub struct FrameSource {
    child: Child,
    stdout: ChildStdout,
}

impl FrameSource {
    pub fn spawn(video: &Path, fps: f64, rotate: Rotate, loop_video: bool) -> io::Result<Self> {
        let filter = build_filter(fps, rotate);
        let mut cmd = Command::new("ffmpeg");
        cmd.arg("-v").arg("error");
        if loop_video {
            cmd.arg("-stream_loop").arg("-1");
        }
        cmd.arg("-i")
            .arg(video)
            .arg("-vf")
            .arg(&filter)
            .arg("-f")
            .arg("rawvideo")
            .arg("-pix_fmt")
            .arg("gray")
            .arg("-");

        let mut child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdout = child.stdout.take().expect("stdout was piped");
        Ok(Self { child, stdout })
    }

    /// Read the next grayscale frame; `Ok(None)` on clean end of video.
    pub fn next_frame(&mut self) -> io::Result<Option<Vec<u8>>> {
        let mut buf = vec![0u8; GRAY_BYTES];
        match read_exact_or_eof(&mut self.stdout, &mut buf)? {
            true => Ok(Some(buf)),
            false => {
                let _ = self.child.wait(); // reap the child
                Ok(None)
            }
        }
    }
}

impl Drop for FrameSource {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Ok(false) only if EOF hits before the first byte; partial frame is an error.
fn read_exact_or_eof(reader: &mut impl Read, buf: &mut [u8]) -> io::Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => {
                if filled == 0 {
                    return Ok(false);
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "ffmpeg produced a partial frame",
                ));
            }
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

fn build_filter(fps: f64, rotate: Rotate) -> String {
    // Portrait panels take 152x296 directly; for rotation we first letterbox
    // the video onto the landscape 296x152 canvas, then transpose.
    let (w, h, transpose) = match rotate {
        Rotate::None => (WIDTH, HEIGHT, None),
        Rotate::Cw => (HEIGHT, WIDTH, Some(1)),
        Rotate::Ccw => (HEIGHT, WIDTH, Some(2)),
    };
    let mut f = format!(
        "fps={fps},scale={w}:{h}:force_original_aspect_ratio=decrease:flags=lanczos,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=white"
    );
    if let Some(t) = transpose {
        f.push_str(&format!(",transpose={t}"));
    }
    f.push_str(",format=gray");
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_geometry() {
        let f = build_filter(0.4, Rotate::None);
        assert!(f.contains("scale=152:296"), "{f}");
        assert!(!f.contains("transpose"), "{f}");
        let f = build_filter(1.0, Rotate::Cw);
        assert!(f.contains("scale=296:152"), "{f}");
        assert!(f.contains("transpose=1"), "{f}");
        assert!(f.ends_with(",format=gray"), "{f}");
        let f = build_filter(1.0, Rotate::Ccw);
        assert!(f.contains("transpose=2"), "{f}");
    }
}
