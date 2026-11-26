//! Atomic file writes and gzip output.

use anyhow::{anyhow, Context, Result};
use flate2::write::GzEncoder;
use flate2::Compression;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Write to "<path>.tmp" and rename over the target, so readers (Emby, the
/// web server) never see a half-written file.
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let mut f = File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(data).with_context(|| format!("writing {}", tmp.display()))?;
        f.sync_all().ok();
    }
    fs::rename(&tmp, path).with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))?;
    Ok(())
}

#[allow(dead_code)]
pub fn gzip(data: &[u8]) -> Result<Vec<u8>> {
    let mut enc = GzEncoder::new(Vec::with_capacity(data.len() / 6), Compression::default());
    enc.write_all(data)?;
    Ok(enc.finish()?)
}

/// Counts what is written (dry runs: the size of the XML without keeping it).
#[derive(Default)]
pub struct Counter {
    pub bytes: usize,
}

impl Write for Counter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.bytes += b.len();
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn tmp_of(p: &Path) -> PathBuf {
    let mut t = p.as_os_str().to_owned();
    t.push(".tmp");
    PathBuf::from(t)
}

/// Writes a guide and its .gz in one pass, straight to temp files that `finish`
/// renames into place. Nothing is kept in memory, so guides of several GB are fine,
/// and readers never see a half-written file.
pub struct GuideWriter {
    xml: std::io::BufWriter<File>,
    gz: GzEncoder<std::io::BufWriter<File>>,
    xml_path: PathBuf,
    gz_path: PathBuf,
    bytes: usize,
}

impl GuideWriter {
    pub fn open(xml_path: &Path, gz_path: &Path) -> Result<GuideWriter> {
        let xt = tmp_of(xml_path);
        let gt = tmp_of(gz_path);
        let xf = File::create(&xt).with_context(|| format!("creating {}", xt.display()))?;
        let gf = File::create(&gt).with_context(|| format!("creating {}", gt.display()))?;
        Ok(GuideWriter {
            xml: std::io::BufWriter::with_capacity(1 << 20, xf),
            gz: GzEncoder::new(std::io::BufWriter::with_capacity(1 << 20, gf), Compression::default()),
            xml_path: xml_path.to_path_buf(),
            gz_path: gz_path.to_path_buf(),
            bytes: 0,
        })
    }

    /// Flush both files and move them into place. Returns (xml bytes, gz bytes).
    pub fn finish(self) -> Result<(usize, usize)> {
        let GuideWriter { xml, gz, xml_path, gz_path, bytes } = self;
        let xt = tmp_of(&xml_path);
        let gt = tmp_of(&gz_path);
        let xf = xml.into_inner().map_err(|e| anyhow!("writing {}: {}", xt.display(), e.error()))?;
        xf.sync_all().ok();
        let gb = gz.finish().with_context(|| format!("writing {}", gt.display()))?;
        let gf = gb.into_inner().map_err(|e| anyhow!("writing {}: {}", gt.display(), e.error()))?;
        gf.sync_all().ok();
        let gz_bytes = fs::metadata(&gt).map(|m| m.len() as usize).unwrap_or(0);
        fs::rename(&xt, &xml_path).with_context(|| format!("renaming {} to {}", xt.display(), xml_path.display()))?;
        fs::rename(&gt, &gz_path).with_context(|| format!("renaming {} to {}", gt.display(), gz_path.display()))?;
        Ok((bytes, gz_bytes))
    }
}

impl Write for GuideWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.xml.write_all(b)?;
        self.gz.write_all(b)?;
        self.bytes += b.len();
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.xml.flush()?;
        self.gz.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guide_writer_writes_both_files() {
        let dir = std::env::temp_dir().join(format!("epgarr-gw-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let (x, g) = (dir.join("a.xml"), dir.join("a.xml.gz"));
        let mut w = GuideWriter::open(&x, &g).unwrap();
        w.write_all(b"<tv></tv>").unwrap();
        let (xb, gb) = w.finish().unwrap();
        assert_eq!(xb, 9);
        assert!(gb > 0);
        assert_eq!(fs::read(&x).unwrap(), b"<tv></tv>");
        let mut d = flate2::read::GzDecoder::new(fs::File::open(&g).unwrap());
        let mut s = String::new();
        std::io::Read::read_to_string(&mut d, &mut s).unwrap();
        assert_eq!(s, "<tv></tv>");
        fs::remove_dir_all(&dir).ok();
    }
}
