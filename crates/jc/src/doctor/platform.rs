//! Which prebuilt tool binaries to download for each OS and CPU.

/// The OS and CPU this binary was built for.
#[derive(Debug, Clone, Copy)]
pub struct Platform {
    pub os: &'static str,
    pub arch: &'static str,
}

impl Platform {
    pub fn current() -> Self {
        Self {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
        }
    }

    pub fn is_windows(self) -> bool {
        self.os == "windows"
    }

    /// `name` with the executable suffix for this OS.
    pub fn exe(self, name: &str) -> String {
        if self.is_windows() {
            format!("{name}.exe")
        } else {
            name.to_string()
        }
    }
}

// ---------------------------------------------------------------------------
// yt-dlp: always the latest release, checked against its SHA2-256SUMS file.
// ---------------------------------------------------------------------------

pub const YT_DLP_REPO: &str = "https://github.com/yt-dlp/yt-dlp";

/// The standalone release asset for this platform.
pub fn yt_dlp_asset(p: Platform) -> Option<&'static str> {
    Some(match (p.os, p.arch) {
        // Universal binary, runs on Apple Silicon and Intel.
        ("macos", _) => "yt-dlp_macos",
        ("linux", "x86_64") => "yt-dlp_linux",
        ("linux", "aarch64") => "yt-dlp_linux_aarch64",
        ("windows", "x86_64") => "yt-dlp.exe",
        ("windows", "aarch64") => "yt-dlp_arm64.exe",
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// ffmpeg + ffprobe: static builds that include libass, freetype and x264.
// ---------------------------------------------------------------------------

/// A pinned file: its URL and SHA-256.
#[derive(Debug, Clone, Copy)]
pub struct PinnedFile {
    pub url: &'static str,
    pub sha256: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub enum FfmpegSource {
    /// Separate ffmpeg and ffprobe zips at fixed URLs with known hashes
    /// (ffmpeg.martin-riedl.de release builds, macOS and Linux).
    Pinned {
        ffmpeg: PinnedFile,
        ffprobe: PinnedFile,
    },
    /// One zip holding both, from a release channel whose hash is read from
    /// the published checksums file (BtbN builds, Windows). The channel
    /// stays on one FFmpeg release branch, but rebuilds keep coming.
    Channel {
        archive_url: &'static str,
        checksums_url: &'static str,
        archive_name: &'static str,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct FfmpegBuild {
    /// Recorded in tools.json; a different id means jc wants another build.
    pub id: &'static str,
    pub version: &'static str,
    pub approx_download_mb: u32,
    pub source: FfmpegSource,
}

macro_rules! martin_riedl {
    ($id:literal, $mb:literal, $ffmpeg_sha:literal, $ffprobe_sha:literal) => {
        FfmpegBuild {
            id: concat!("martin-riedl/", $id),
            version: "9.1",
            approx_download_mb: $mb,
            source: FfmpegSource::Pinned {
                ffmpeg: PinnedFile {
                    url: concat!(
                        "https://ffmpeg.martin-riedl.de/download/",
                        $id,
                        "/ffmpeg.zip"
                    ),
                    sha256: $ffmpeg_sha,
                },
                ffprobe: PinnedFile {
                    url: concat!(
                        "https://ffmpeg.martin-riedl.de/download/",
                        $id,
                        "/ffprobe.zip"
                    ),
                    sha256: $ffprobe_sha,
                },
            },
        }
    };
}

macro_rules! btbn {
    ($name:literal, $mb:literal) => {
        FfmpegBuild {
            id: concat!("btbn/", $name),
            version: "9.0",
            approx_download_mb: $mb,
            source: FfmpegSource::Channel {
                archive_url: concat!(
                    "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/",
                    $name
                ),
                checksums_url: "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/checksums.sha256",
                archive_name: $name,
            },
        }
    };
}

pub fn ffmpeg_build(p: Platform) -> Option<FfmpegBuild> {
    Some(match (p.os, p.arch) {
        ("macos", "aarch64") => martin_riedl!(
            "macos/arm64/1795207104_9.1",
            57,
            "1f66e9a690a7f1c9e60b012379f8869c11818d1797f45b21dbb2f2564ba61ae1",
            "6ef2df5f575042923a28537ab37adf936f28bf3e7b85de23a1447ed447ec1dbd"
        ),
        ("macos", "x86_64") => martin_riedl!(
            "macos/amd64/1795212433_9.1",
            68,
            "35b3d82cccc2c1d644d49d15f5e57eb913a08ef68f798042c90eecec62c51f1e",
            "41ca7b7cba7992e33f1da3f1bf54db8ec103a4d387b1bfac3162062e8b1074ad"
        ),
        ("linux", "x86_64") => martin_riedl!(
            "linux/amd64/1795205506_9.1",
            67,
            "707d4fa675932cc310e39e1506ac9224049786119689e345fed1c453a8a0896e",
            "5e4c53c9c86d6371027efa83d62c2cea00e02ded0234d8cc952d06c9259e4b33"
        ),
        ("linux", "aarch64") => martin_riedl!(
            "linux/arm64/1795204341_9.1",
            58,
            "e42242dd20b01cf9fe6cff1902b6e5e72be07c2b551d95abc8f332cdb0ff7d86",
            "a1561aa2136bce9f51dac9b13be4480466a3031f82907d551619dcdc3b867ccc"
        ),
        ("windows", "x86_64") => btbn!("ffmpeg-n9.0-latest-win64-gpl-9.0.zip", 200),
        ("windows", "aarch64") => btbn!("ffmpeg-n9.0-latest-winarm64-gpl-9.0.zip", 140),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHIPPED: [(&str, &str); 5] = [
        ("macos", "aarch64"),
        ("macos", "x86_64"),
        ("linux", "x86_64"),
        ("linux", "aarch64"),
        ("windows", "x86_64"),
    ];

    #[test]
    fn every_shipped_platform_has_tools() {
        for (os, arch) in SHIPPED {
            let p = Platform { os, arch };
            assert!(yt_dlp_asset(p).is_some(), "yt-dlp for {os} {arch}");
            assert!(ffmpeg_build(p).is_some(), "ffmpeg for {os} {arch}");
        }
    }

    #[test]
    fn pinned_hashes_look_like_sha256() {
        for (os, arch) in SHIPPED {
            if let Some(FfmpegBuild {
                source: FfmpegSource::Pinned { ffmpeg, ffprobe },
                ..
            }) = ffmpeg_build(Platform { os, arch })
            {
                for f in [ffmpeg, ffprobe] {
                    assert_eq!(f.sha256.len(), 64);
                    assert!(f.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
                    assert!(f.url.starts_with("https://"));
                }
            }
        }
    }

    #[test]
    fn unknown_platform_has_none() {
        let p = Platform {
            os: "freebsd",
            arch: "x86_64",
        };
        assert!(yt_dlp_asset(p).is_none());
        assert!(ffmpeg_build(p).is_none());
    }
}
