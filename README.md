# just-clipping

> Turn long videos and livestreams into short vertical clips that are ready to post, with Claude Code.

Paste a YouTube or Twitch link (or give a local file). just-clipping finds the best moments, lets you pick, reframes them to 9:16 with animated captions, writes covers and descriptions for each platform, and hands you a folder ready to upload to TikTok, YouTube Shorts, Instagram Reels or X.

> **Status: early design.** Nothing is usable yet. This README describes what's being built.

---

## How it works

```
/clip https://www.twitch.tv/videos/123456
```

1. **Ingest:** downloads the video, its chat replay and, for YouTube, the "most replayed" heatmap.
2. **Analyze:** transcript with word-level timing, chat activity and hype, audio peaks, scene changes.
3. **Find moments:** the strongest moments get ranked and trimmed to a tight hook and payoff.
4. **You pick the clips** on a local review page.
5. **Render:** vertical 1080×1920 with a layout suited to the content, animated word-by-word captions, loudness normalized.
6. **Package:** several cover and title/description/hashtag options per platform.
7. **You pick the covers and copy** on the review page.
8. **Export:** one folder per clip with the video, cover and a text file per platform.

Every step is resumable: run `/clip` again on the same source and it picks up where it left off.

## Supported

| | MVP | Planned |
|---|---|---|
| **Sources** | YouTube VODs, Twitch VODs, local files | YouTube and Twitch live streams |
| **Languages** | English, French | More languages added as data files (es, it, de, ja, …) |
| **Content** | Gaming (facecam split layout), generic fallback | Podcasts (follow the speaker), IRL (follow the subject) |
| **Platforms** | TikTok, YouTube Shorts, Instagram Reels, X | Automatic posting |
| **Long form** | — | Compilations built from your clip library |

Captions always stay in the language of the source.

## Architecture

just-clipping has two parts:

- **A Claude Code plugin** (`/clip` skill) for the judgment: which moments are actually good, where to cut, which layout fits, which cover and copy to use. Claude Code is the AI here, so **no API key is needed**.
- **`jc`, a single Rust binary** for everything else: downloading, transcription, signal analysis, face detection, cutting, reframing, captions, rendering, the review page.

The two exchange JSON files in a project folder, so every `jc` step can also be run by hand.

`jc` relies on a few tools it downloads and manages itself:

- [yt-dlp](https://github.com/yt-dlp/yt-dlp) for downloading
- [ffmpeg](https://ffmpeg.org/) for cutting and rendering
- [whisper.cpp](https://github.com/ggerganov/whisper.cpp) models for transcription, on the GPU where available
- a small ONNX face-detection model

## Requirements (planned)

- macOS, Linux or Windows
- [Claude Code](https://claude.com/claude-code)
- The `jc` binary. `jc doctor` fetches everything else on first run.
- Optional: a GPU (Apple Silicon, NVIDIA or Vulkan) for much faster transcription
- Optional: Docker, for running on a server or recording live streams 24/7

## Roadmap

- [x] **M0:** project skeleton, `jc doctor`, project manifest
- [ ] **M1:** ingest and analyze (transcript, chat, heatmap, audio, scenes)
- [ ] **M2:** candidate moments, `/clip` ranking, review page
- [ ] **M3:** vertical render: layouts, en/fr animated captions, loudness
- [ ] **M4:** covers, platform copy, export
- [ ] **M5:** prebuilt binaries, plugin install, Dockerfile
- [ ] **Phase 2:** live mode, podcast and IRL layouts, more languages
- [ ] **Phase 3:** compilations, automatic posting

## Development

You need a recent stable Rust toolchain.

```bash
cargo build
cargo test
cargo run -- doctor        # downloads yt-dlp and ffmpeg into the cache
cargo run -- status        # lists projects in the workspace
```

- **Workspace** (projects, `config.toml`): `~/JustClipping`, or `--workspace` / `JC_WORKSPACE`.
- **Cache** (tools, models): the OS cache folder, or `JC_CACHE_DIR`.
- **Every command takes `--json`**, which prints only the result on stdout. Progress goes to stderr.
- **Exit codes:** 0 ok, 1 error, 2 bad arguments, 3 not implemented yet, 4 setup incomplete (`jc doctor`), 5 project not found.

`jc doctor` uses the ffmpeg on your PATH only if it has libass, libx264 and the loudness filters. Otherwise it downloads a pinned, checksummed static build. yt-dlp is always jc's own copy, so jc can keep it up to date.

`spikes/native-deps` is a separate build check for whisper.cpp and the ONNX face-detection backends. Its CI workflow runs on all platforms.

## A note on rights

just-clipping is a tool for clipping content you own or have permission to clip, such as streamers you clip for. Downloading and reposting other people's videos without permission may break platform terms of service and copyright law. You are responsible for what you publish.
