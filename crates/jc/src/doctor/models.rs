//! Models jc downloads on demand: whisper (ggml) for transcription and YuNet
//! (ONNX) for face detection. Every file is pinned by URL and SHA-256.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy)]
pub struct ModelSpec {
    /// Name used in config.toml and reports.
    pub id: &'static str,
    pub file: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
}

impl ModelSpec {
    pub fn path(&self, models_dir: &Path) -> PathBuf {
        models_dir.join(self.file)
    }

    pub fn approx_mb(&self) -> u64 {
        self.bytes.div_ceil(1_000_000)
    }

    /// Present with the expected size. Hashes are checked at download time
    /// only, since re-hashing 500 MB on every run is slow.
    pub fn is_installed(&self, models_dir: &Path) -> bool {
        std::fs::metadata(self.path(models_dir)).is_ok_and(|m| m.len() == self.bytes)
    }
}

macro_rules! whisper {
    ($id:literal, $sha:literal, $bytes:literal) => {
        ModelSpec {
            id: $id,
            file: concat!("ggml-", $id, ".bin"),
            url: concat!(
                "https://huggingface.co/ggerganov/whisper.cpp/resolve/",
                "5359861c739e955e79d9a303bcbc70fb988958b1/ggml-",
                $id,
                ".bin"
            ),
            sha256: $sha,
            bytes: $bytes,
        }
    };
}

pub const WHISPER_MODELS: [ModelSpec; 5] = [
    whisper!(
        "tiny",
        "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
        77691713
    ),
    whisper!(
        "base",
        "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
        147951465
    ),
    whisper!(
        "small",
        "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
        487601967
    ),
    whisper!(
        "large-v3-turbo-q5_0",
        "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
        574041195
    ),
    whisper!(
        "large-v3-turbo",
        "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69",
        1624555275
    ),
];

pub const YUNET: ModelSpec = ModelSpec {
    id: "yunet-2023mar",
    file: "face_detection_yunet_2023mar.onnx",
    url: "https://github.com/opencv/opencv_zoo/raw/f12e12798e8314f7c074a6656816c048dcc95b7a/models/face_detection_yunet/face_detection_yunet_2023mar.onnx",
    sha256: "8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4",
    bytes: 232589,
};

/// Silero voice activity detection for whisper.cpp: transcription skips
/// silence and music, which is faster and stops whisper from inventing text
/// there.
pub const SILERO_VAD: ModelSpec = ModelSpec {
    id: "silero-v6.2.0",
    file: "ggml-silero-v6.2.0.bin",
    url: "https://huggingface.co/ggml-org/whisper-vad/resolve/9ffd54a1e1ee413ddf265af9913beaf518d1639b/ggml-silero-v6.2.0.bin",
    sha256: "2aa269b785eeb53a82983a20501ddf7c1d9c48e33ab63a41391ac6c9f7fb6987",
    bytes: 885098,
};

pub fn whisper_model(id: &str) -> Option<ModelSpec> {
    WHISPER_MODELS.iter().copied().find(|m| m.id == id)
}

pub fn whisper_ids() -> Vec<&'static str> {
    WHISPER_MODELS.iter().map(|m| m.id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_model_is_known() {
        let default = crate::config::ModelsConfig::default().whisper;
        assert!(whisper_model(&default).is_some(), "{default}");
    }

    #[test]
    fn specs_are_well_formed() {
        for m in WHISPER_MODELS.iter().chain([&YUNET, &SILERO_VAD]) {
            assert_eq!(m.sha256.len(), 64, "{}", m.id);
            assert!(
                m.url.starts_with("https://") && m.url.ends_with(m.file),
                "{}",
                m.id
            );
        }
    }
}
