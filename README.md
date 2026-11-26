<div align="center">

<img src="assets/logo/vectrcap.svg" width="96" alt="VectrCap logo">

# VectrCap

**Screenshots, annotation, screen recording and uploads to your own [Vectr](https://github.com/erebuste/vectr) server — from the Windows tray.**

[![Latest release](https://img.shields.io/github/v/release/erebuste/VectrCap?style=flat-square&label=release)](https://github.com/erebuste/VectrCap/releases/latest)
[![CI](https://img.shields.io/github/actions/workflow/status/erebuste/VectrCap/ci.yml?branch=main&style=flat-square&label=checks)](https://github.com/erebuste/VectrCap/actions/workflows/ci.yml)
![Windows 11 x64](https://img.shields.io/badge/Windows%2011-x64-0078D4?style=flat-square&logo=windows11&logoColor=white)
![Rust 1.98.1](https://img.shields.io/badge/Rust-1.98.1-B7410E?style=flat-square&logo=rust&logoColor=white)

[**Download**](https://github.com/erebuste/VectrCap/releases/latest) ·
[Getting started](#-getting-started) ·
[Features](#-capturing-and-uploading) ·
[Recording](#-recording) ·
[Building](#-building-from-source)

</div>

---

## ✨ Highlights

|   |   |
|---|---|
| 📸 **Capture** | Select a region, window, control or client area; scrolling capture, delay and cursor capture |
| ✏️ **Annotate** | Mark up before you share; redaction is solid and flattened into the output |
| 🎬 **Record** | GIF or MP4 (H.264) with system audio, microphone or both, using codecs built into Windows |
| ☁️ **Upload** | To your self-hosted Vectr server, with a resumable queue that never uploads the same file twice |
| 📌 **Pin** | Keep screenshots floating on screen; move, zoom, fade or minimize them |
| 🔄 **Self-updating** | Portable single exe that updates itself from GitHub Releases after verifying the checksum |

## 🤝 Made for Vectr

VectrCap and [Vectr](https://github.com/erebuste/vectr) are designed as a pair. Vectr is the self-hosted file server and VectrCap is the Windows capture client that uploads to it.

- Vectr's panel hands out a ready-made `.vectrcap` connection file (**Uploaders → Download VectrCap config**).
- VectrCap checks the token against Vectr's `GET /api/uploader`.
- VectrCap uses the link domains and per-upload settings that Vectr offers.

> [!NOTE]
> VectrCap runs on **Windows 11 x64**. It is a portable, unsigned build with no installer, and it [updates itself](#-updates).

## 🚀 Getting started

1. **Run `VectrCap.exe`.** The first run opens Settings, and after that the app lives in the tray.
   Left-click the tray icon to capture a region, or right-click it for the menu.
   Closing the window returns it to the tray. **Quit** finishes accepted work, then exits.
2. **Connect to Vectr.** Open the `.vectrcap` file downloaded from Vectr's panel, check which server it points to and confirm.
   You can also add a destination by hand with a name, the HTTPS server address and an upload token.
3. **Press Test connection** to check the token. Tokens are stored in Windows Credential Manager, never in the settings file.
4. **Pick the default destination** and, optionally, which of the server's link domains to use.

## 📸 Capturing and uploading

- **Select.** Hovering highlights a window, control or client area. Click to select it, or drag to select a region.
  Adjust the selection and annotate, then choose **Upload**, **Copy**, **Save** or **Pin**. Selecting alone never uploads.
- **Options.** Settings cover:
  - a screenshot delay of 0–60 s
  - reusing the last region
  - capturing the cursor
  - PNG, JPEG or WebP output, with quality and resize previews and configurable file names
- **Scrolling capture.** Keep the window still while you scroll down. You review the stitched result before using it.
- **Pins.** Each pinned screenshot floats on screen. You can move, zoom, minimize it or change its opacity.
- **Upload files.**
  - From Explorer: **Show more options** on Windows 11.
  - By dropping them onto VectrCap.
  - From the clipboard.

  Your original files are never changed.
- **Tools.** The color picker copies HEX or RGB, and the ruler measures in physical pixels.
- **Hotkeys.** Global hotkeys start unassigned. Set them in **Settings → Shortcuts**. Automatic upload after capture is opt-in.
- **History.** Shows every capture and upload as a gallery or list.
  **Remove from history** forgets a record, and **Delete local file** deletes VectrCap's own saved copy.
  Neither deletes anything on the server.
- **Queue.** You can pause uploads. They resume after a restart without uploading the same file twice.

## 🎬 Recording

A recording region must fit on one monitor. A countdown runs first (3 seconds by default, adjustable from 0 to 60), and you can pause and resume.

| Format | FPS | Default limit | Range | Audio |
|---|---|---|---|---|
| **GIF** | 10 / 15 / 20 (default 15) | 60 seconds | 1–300 seconds | — |
| **MP4** (H.264) | 15 / 30 / 60 (default 30) | 10 minutes | 1–60 minutes | Off by default; system, microphone or both |

MP4 quality is Small, Balanced (the default), High or a custom 0.5–100 Mbps. Video is kept at its original size or scaled down to 720p or 1080p.

## 📁 Where your files are

| Location | Contents |
|---|---|
| `%LOCALAPPDATA%\VectrCap\settings.json` | Settings (no tokens) |
| `%LOCALAPPDATA%\VectrCap\history.db` | History and the upload queue |
| `%LOCALAPPDATA%\VectrCap\spool\` | Files waiting to upload |
| `Pictures\VectrCap\YYYY-MM\` | Saved screenshots and recordings |

- **What gets saved.** By default every capture is saved. You can instead keep only pending and failed uploads, or save nothing automatically.
- **Cleanup.** Automatic cleanup of old captures is off by default. It only removes files that VectrCap saved itself and that haven't changed.
- **Backups.** You can back up settings to a JSON file. Backups never include tokens or history.

> [!IMPORTANT]
> Removing local data does not delete files or tokens on the Vectr server. Manage those in Vectr.

## 🔄 Updates

VectrCap checks GitHub Releases for a newer version shortly after it starts, then once a day. **Settings → General → Updates** shows the result and has a **Check now** button.

Nothing installs on its own. When a new version is available, choose **Update and restart**, either in Settings or in the tray menu. VectrCap then:

1. downloads the new `VectrCap.exe`
2. checks it against the release's SHA-256 checksum
3. closes, after finishing accepted work
4. swaps in the new exe and starts again

The previous exe is kept as `VectrCap.exe.old` until the next start.

## 🛠️ Troubleshooting

**Settings → Troubleshooting** shows queue and storage status, tests the connection to a destination and can create a diagnostic report to save or copy.

> [!TIP]
> The diagnostic report contains only counts and error codes. It has no paths, links, names or tokens, and it is never sent anywhere automatically.

| Problem | Fix |
|---|---|
| Token missing or rejected | Set the token again for that destination |
| Certificate or redirect errors | Fix the server's HTTPS setup |
| Link didn't reach the clipboard | VectrCap offers to copy it again without re-uploading |

## 🧱 Building from source

<details>
<summary><b>Requirements and commands</b></summary>

<br>

Building requires:

- Windows 11 x64
- Rust **1.98.1** (MSVC, with rustfmt and clippy)
- Visual Studio C++ build tools
- a Windows SDK

```powershell
rustup toolchain install 1.98.1 --profile minimal --component rustfmt --component clippy --target x86_64-pc-windows-msvc
cargo test --all-targets
./scripts/build-release.ps1   # builds target/packages/VectrCap-x64/
```

`build-release.ps1` closes a running packaged copy of VectrCap. It then replaces `target/packages/VectrCap-x64/` with the exe, this README and the third-party notices.

- After changing dependencies, run `./scripts/generate-notices.ps1` to refresh `assets/THIRD-PARTY-NOTICES.md`.
- `python scripts/render-logo.py` generates the logo.

</details>

<details>
<summary><b>Publishing a release</b></summary>

<br>

1. Set the new `version` in `Cargo.toml` and commit.
2. Push a matching tag:

   ```powershell
   git tag v0.2.0
   git push origin v0.2.0
   ```

The Release workflow runs the tests, builds the exe and publishes `VectrCap.exe`, its `.sha256` and a zip of the package. The in-app updater picks it up from there.

</details>
