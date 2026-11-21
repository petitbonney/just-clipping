//! HTTP downloads with SHA-256 verification, and zip extraction.

use anyhow::{Context, Result, bail};
use indicatif::{ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct Http {
    client: reqwest::blocking::Client,
    no_redirect: reqwest::blocking::Client,
}

fn builder() -> reqwest::blocking::ClientBuilder {
    reqwest::blocking::Client::builder()
        .user_agent(concat!("just-clipping/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(30))
        // Large files: no overall deadline.
        .timeout(None)
}

impl Http {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: builder().build().context("setting up HTTP")?,
            no_redirect: builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .context("setting up HTTP")?,
        })
    }

    pub fn get_text(&self, url: &str) -> Result<String> {
        self.client
            .get(url)
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.text())
            .with_context(|| format!("fetching {url}"))
    }

    /// The `Location` a URL redirects to, without following it.
    pub fn redirect_target(&self, url: &str) -> Result<String> {
        let resp = self
            .no_redirect
            .get(url)
            .send()
            .with_context(|| format!("fetching {url}"))?;
        if !resp.status().is_redirection() {
            bail!("expected a redirect from {url}, got {}", resp.status());
        }
        let location = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .with_context(|| format!("no redirect target from {url}"))?;
        Ok(location.to_string())
    }

    /// Downloads `url` to `dest`, checking its SHA-256. The file is written
    /// next to `dest` first and only moved into place once verified.
    pub fn download(&self, url: &str, dest: &Path, sha256: &str, label: &str) -> Result<()> {
        let part = with_suffix(dest, ".part");
        let result = self.download_to(url, &part, sha256, label);
        if result.is_err() {
            let _ = std::fs::remove_file(&part);
        }
        result?;
        std::fs::rename(&part, dest).with_context(|| format!("moving to {}", dest.display()))
    }

    fn download_to(&self, url: &str, path: &Path, sha256: &str, label: &str) -> Result<()> {
        let mut resp = self
            .client
            .get(url)
            .send()
            .and_then(|r| r.error_for_status())
            .with_context(|| format!("downloading {url}"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let mut file =
            File::create(path).with_context(|| format!("creating {}", path.display()))?;

        let bar = match resp.content_length() {
            Some(len) => ProgressBar::new(len).with_style(
                ProgressStyle::with_template(
                    "  {msg:<18} [{bar:30}] {bytes}/{total_bytes} {bytes_per_sec}",
                )
                .expect("valid template")
                .progress_chars("=> "),
            ),
            None => ProgressBar::new_spinner(),
        };
        bar.set_message(label.to_string());

        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let n = resp
                .read(&mut buf)
                .with_context(|| format!("downloading {url}"))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n])
                .with_context(|| format!("writing {}", path.display()))?;
            bar.inc(n as u64);
        }
        bar.finish_and_clear();
        file.sync_all().ok();

        let actual = hex(&hasher.finalize());
        if !actual.eq_ignore_ascii_case(sha256) {
            bail!("checksum mismatch for {url}: expected {sha256}, got {actual}");
        }
        Ok(())
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("reading {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

/// Finds the hash for `name` in a `sha256sum`-style file
/// (`<hash>  <name>` or `<hash> *<name>` per line).
pub fn parse_sha256sums(text: &str, name: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (hash, file) = line.trim().split_once(char::is_whitespace)?;
        let file = file.trim_start().trim_start_matches('*');
        (file == name && hash.len() == 64).then(|| hash.to_ascii_lowercase())
    })
}

/// Extracts the entry whose file name is `file_name` (in any folder of the
/// archive) to `dest`.
pub fn extract_from_zip(zip_path: &Path, file_name: &str, dest: &Path) -> Result<()> {
    let file = File::open(zip_path).with_context(|| format!("opening {}", zip_path.display()))?;
    let mut archive =
        zip::ZipArchive::new(file).with_context(|| format!("reading {}", zip_path.display()))?;
    let index = (0..archive.len())
        .find(|&i| {
            archive
                .name_for_index(i)
                .and_then(Result::ok)
                .is_some_and(|name| {
                    !name.ends_with('/') && name.rsplit('/').next() == Some(file_name)
                })
        })
        .with_context(|| format!("{file_name} not found in {}", zip_path.display()))?;
    let mut entry = archive.by_index(index)?;
    let part = with_suffix(dest, ".part");
    let mut out = File::create(&part).with_context(|| format!("creating {}", part.display()))?;
    std::io::copy(&mut entry, &mut out)
        .with_context(|| format!("extracting {file_name} from {}", zip_path.display()))?;
    drop(out);
    make_executable(&part)?;
    std::fs::rename(&part, dest).with_context(|| format!("moving to {}", dest.display()))
}

#[cfg(unix)]
pub fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .with_context(|| format!("making {} executable", path.display()))
}

#[cfg(not(unix))]
pub fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

pub fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUMS: &str = "\
e51e69e89489f199530f13d32a4342da3ddb2cf34842f364603a066db05afbf3  yt-dlp
f5389d3698ecde4ef06f4e6b1ce0294fe9ac1396228c8990b8d3416d6d177a7d  yt-dlp.exe
0F922C15CCC0B83EEFA1C8BC9104E434318C4BF16E408E1EE93B7745A6EFE0AA *yt-dlp_macos
";

    #[test]
    fn finds_exact_name_only() {
        assert_eq!(
            parse_sha256sums(SUMS, "yt-dlp").unwrap(),
            "e51e69e89489f199530f13d32a4342da3ddb2cf34842f364603a066db05afbf3"
        );
        assert!(parse_sha256sums(SUMS, "yt-dlp.ex").is_none());
    }

    #[test]
    fn handles_binary_marker_and_case() {
        assert_eq!(
            parse_sha256sums(SUMS, "yt-dlp_macos").unwrap(),
            "0f922c15ccc0b83eefa1c8bc9104e434318c4bf16e408e1ee93b7745a6efe0aa"
        );
    }

    #[test]
    fn hashes_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn extracts_nested_entry_by_file_name() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("a.zip");
        {
            let mut w = zip::ZipWriter::new(File::create(&zip_path).unwrap());
            let opts = zip::write::SimpleFileOptions::default();
            w.add_directory("ffmpeg-n9.0/bin/", opts).unwrap();
            w.start_file("ffmpeg-n9.0/bin/ffmpeg.exe", opts).unwrap();
            w.write_all(b"binary").unwrap();
            w.start_file("ffmpeg-n9.0/bin/ffprobe.exe", opts).unwrap();
            w.write_all(b"probe").unwrap();
            w.finish().unwrap();
        }
        let dest = dir.path().join("ffmpeg.exe");
        extract_from_zip(&zip_path, "ffmpeg.exe", &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"binary");
        assert!(extract_from_zip(&zip_path, "missing", &dest).is_err());
    }
}
