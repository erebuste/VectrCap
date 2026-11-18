<img src="assets/logo/vectrcap.svg" width="72" alt="VectrCap logo">

# VectrCap

Windows tray app for screenshots, annotation, screen recording and uploads to your own Vectr server.

## Made for Vectr

VectrCap and [Vectr](https://github.com/erebuste/vectr) are designed to work together and work best as a pair: Vectr is the
self-hosted file server, VectrCap the Windows capture client that uploads to it. Vectr's panel hands out a ready-made
`.vectrcap` connection file (**Uploaders → Download VectrCap config**), VectrCap checks the token against Vectr's
`GET /api/uploader` and uses the link domains and per-upload settings Vectr offers. Both share the same logo family.

Runs on **Windows 11 x64**. VectrCap is a portable, unsigned build: there is no installer or automatic updater yet.

## Getting started

1. Start `VectrCap.exe`. The first run opens Settings; afterwards the app lives in the tray. Left-click the tray icon to
   capture a region, right-click for the menu. Closing the window returns to the tray; **Quit** finishes accepted work and exits.
2. Connect to Vectr: open the `.vectrcap` file downloaded from Vectr's panel, check the server it points to and confirm.
   Or add a destination by hand with a name, the HTTPS server address and an upload token.
3. Press **Test connection** to check the token. Tokens are stored in Windows Credential Manager, never in the settings file.
4. Pick the default destination and, if you like, which of the server's link domains to use.

## Capturing and uploading

- Hover highlights a window, click selects it, drag selects a region. Adjust the selection and annotate, then choose
  **Upload**, **Copy**, **Save** or **Pin**. Selecting alone never uploads. Redaction is solid and flattened into the output.
- Screenshot delay (0–60 s), reuse of the last region and cursor capture are in Settings. Output can be PNG, JPEG or WebP,
  with quality and resize previews and configurable file names.
- Scrolling capture: keep a window still while you scroll down, then review the stitched result before using it.
- Pins keep a screenshot floating on screen; move, zoom, change opacity or minimize each one.
- Upload files from Explorer (**Show more options** on Windows 11), by dropping them, or from the clipboard.
  Your original files are never changed.
- The color picker copies HEX or RGB; the ruler measures in physical pixels.
- Global hotkeys start unassigned; set them in **Settings → Shortcuts**. Automatic upload after capture is opt-in.
- **History** shows every capture and upload as a gallery or list. **Remove from history** forgets a record;
  **Delete local file** deletes VectrCap's own saved copy. Neither deletes anything on the server.
- Uploads can be paused and resume after a restart without uploading the same file twice.

## Recording

Recording regions must fit on one monitor. A 3-second countdown (0–60 s) runs first; Pause/Resume is supported.

| Format | FPS | Default length limit | Maximum | Audio |
|---|---|---|---|---|
| GIF | 10 / 15 / 20 (default 15) | 60 seconds | 1–300 seconds | None |
| MP4 (H.264) | 15 / 30 / 60 (default 30) | 10 minutes | 1–60 minutes | Off by default; system, microphone, or both |

MP4 quality is Small, Balanced (default), High or a custom 0.5–100 Mbps, at original size or scaled down to 720p or 1080p.
Recording uses the codecs built into Windows; nothing extra is installed.

## Where your files are

| Location | Contents |
|---|---|
| `%LOCALAPPDATA%\VectrCap\settings.json` | Settings (no tokens) |
| `%LOCALAPPDATA%\VectrCap\history.db` | History and the upload queue |
| `%LOCALAPPDATA%\VectrCap\spool\` | Files waiting to upload |
| `Pictures\VectrCap\YYYY-MM\` | Saved screenshots and recordings |

By default every capture is saved; you can instead keep only pending/failed uploads or save nothing automatically.
Automatic cleanup of old captures is off by default and only ever removes files VectrCap saved itself and that are unchanged.
Settings can be backed up to a JSON file; backups never include tokens or history. Removing local data does not delete
files or tokens on the Vectr server; manage those in Vectr.

## Troubleshooting

**Settings → Troubleshooting** shows queue and storage status, tests the connection to a destination and can create a
diagnostic report to save or copy. The report contains only counts and error codes: no paths, links, names or tokens,
and it is never sent anywhere automatically.

If the token is missing or rejected, set it again for the destination. For certificate or redirect errors, fix the server's
HTTPS setup. If a link fails to reach the clipboard, VectrCap offers to copy it again without re-uploading.

## Building from source

Requires Windows 11 x64, Rust **1.98.1** (MSVC, with rustfmt and clippy), Visual Studio C++ build tools and a Windows SDK.

```powershell
rustup toolchain install 1.98.1 --profile minimal --component rustfmt --component clippy --target x86_64-pc-windows-msvc
cargo test --all-targets
./scripts/build-release.ps1   # builds target/packages/VectrCap-x64/
```

`build-release.ps1` closes a running packaged VectrCap and replaces `target/packages/VectrCap-x64/` with the exe, this
README and the third-party notices. After changing dependencies, run `./scripts/generate-notices.ps1` to refresh
`assets/THIRD-PARTY-NOTICES.md`. The logo is generated by `python scripts/render-logo.py`.
