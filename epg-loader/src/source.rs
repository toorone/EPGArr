//! Fetching the XMLTV source and undoing gzip.

use anyhow::{anyhow, bail, Context, Result};
use flate2::read::MultiGzDecoder;
use std::io::Read;
use std::path::Path;

/// True when the bytes start with the gzip magic number.
pub fn is_gzip(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b
}

/// Gunzip if needed and strip a UTF-8 byte order mark.
pub fn decode(bytes: Vec<u8>) -> Result<Vec<u8>> {
    let mut out = if is_gzip(&bytes) {
        let mut d = MultiGzDecoder::new(&bytes[..]);
        let mut out = Vec::with_capacity(bytes.len() * 8);
        d.read_to_end(&mut out).context("decompressing gzipped source")?;
        out
    } else {
        bytes
    };
    if out.starts_with(&[0xEF, 0xBB, 0xBF]) {
        out.drain(..3);
    }
    Ok(out)
}

/// Download an http(s) URL or read a local file. Error messages never include
/// the URL, because IPTV source URLs often carry a username and password.
pub async fn fetch(client: &reqwest::Client, src: &str, base: &Path) -> Result<Vec<u8>> {
    let lower = src.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        let resp = client
            .get(src)
            .send()
            .await
            .map_err(|e| anyhow!("downloading source_url: {}", e.without_url()))?;
        let status = resp.status();
        if !status.is_success() {
            bail!("downloading source_url: HTTP {status}");
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| anyhow!("downloading source_url: {}", e.without_url()))?;
        Ok(bytes.to_vec())
    } else {
        let p = src.strip_prefix("file://").unwrap_or(src);
        let p = Path::new(p);
        let p = if p.is_absolute() { p.to_path_buf() } else { base.join(p) };
        tokio::fs::read(&p)
            .await
            .with_context(|| format!("reading source file {}", p.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;

    const SAMPLE: &[u8] = include_bytes!("../tests/sample.xml");

    #[test]
    fn detects_plain_xml() {
        assert!(!is_gzip(SAMPLE));
        assert!(!is_gzip(b""));
        assert!(!is_gzip(&[0x1f]));
        assert_eq!(decode(SAMPLE.to_vec()).unwrap(), SAMPLE);
    }

    #[test]
    fn detects_and_decodes_gzip() {
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(SAMPLE).unwrap();
        let gz = enc.finish().unwrap();
        assert!(is_gzip(&gz));
        assert_eq!(decode(gz).unwrap(), SAMPLE);
    }

    #[test]
    fn strips_bom() {
        let mut v = vec![0xEF, 0xBB, 0xBF];
        v.extend_from_slice(b"<tv/>");
        assert_eq!(decode(v).unwrap(), b"<tv/>");
    }
}
