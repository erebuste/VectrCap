# VectrCap

Windows tray capture, annotation, recording and explicit uploads to Vectr. This is an **unsigned portable development build**. There is no installer, code signing, automatic updater or claim of completed Windows host acceptance. Signing and installer selection are separate delivery decisions.

The supported target is **Windows 11 x64** with the Microsoft Media Foundation codecs available. Automated builds/tests have run on Windows NT 10.0.26100 (Windows 11); this establishes headless engineering coverage, not hardware, UI, accessibility or live-server certification. See the [unexercised Windows acceptance checklist](docs/verification/windows-acceptance.md) and [Task 14 verification report](docs/verification/task14-report.md).

## First connection

1. Start `VectrCap.exe`. First run opens Settings; afterward the app lives in the tray. Closing the window returns to the tray; **Quit** drains accepted work and exits.
2. Add a destination with a name, an HTTPS **server origin** (no API path/query), and its Vectr upload token. Tokens are stored in Windows Credential Manager, separately per destination and origin, never in settings JSON. Multiple destinations on one origin have independent tokens.
3. Use **Test connection**. `GET /api/uploader` with the upload token verifies uploader access when supported. Only a 404 permits **unverified/manual-domain compatibility**; 401, malformed responses, TLS and other errors do not. TLS verification stays enabled and redirects are refused.
4. Choose the default destination and, if needed, an explicit allowed link-domain override. Inherit uses the server's domain choice without an override header. Existing accepted work keeps its original destination/credential/domain snapshot.

You can also explicitly import a `.vectrcap` destination installation file, review its origin before network verification, and confirm installation. Settings backup JSON is a separate, secret-free format; imported new connections need local tokens. OS file association and Explorer integration are opt-in. Their real Windows activation and the actual Vectr endpoint/panel download still require host verification; fixture support is not evidence that a server feature is deployed.

## Controls and output

- Use the tray menu for capture, recordings, uploads, History, tools and Settings. Assign or disable global action hotkeys in Settings; global actions start unassigned. Editor/tool shortcuts are configurable separately and respect text focus.
- Hover highlights a window; click selects it; drag selects a region. Adjust selection and annotate before explicitly choosing **Upload**, **Copy**, **Save** or **Pin**. Selection alone does not upload. Redaction is solid and flattened into output. Screenshots can span monitors within the 64-megapixel capture budget.
- Configure screenshot delay (0–60 seconds), last-region reuse and cursor inclusion (initially off). PNG/JPEG/WebP output has format/quality/resize previews; encoder-owned extensions and collision-safe filename presets apply to finalized captures.
- Manual scrolling capture keeps a fixed window/viewport while **you** scroll downward. Capture steps, inspect stitch quality, and explicitly review partial/uncertain results before shared annotation/output actions.
- Pins hold finalized still images, with independent move/zoom/opacity/minimize controls. Limits: 16 pins, 128 megapixels total, 64 megapixels per image and 128 MiB encoded input. Capture exclusion across real windows/monitors remains a host check.
- Upload selected regular files, Explorer selections, explicit drops or clipboard sources. Explorer's Windows 11 integration uses **Show more options**. There is no clipboard monitoring, archive extraction or URL fetching from pasted text. Originals stay intact.
- Screen sampling opens a result with separate **Copy HEX** / **Copy RGB** actions. The ruler reports physical-pixel measurements. These utilities do not create uploads or capture archives.
- History shares Gallery/List selection, preview, favorite and item actions. **Remove from history** removes a record. **Delete local file** is a separate action restricted to a recorded unchanged app-owned copy; pending work blocks deletion. Confirmed remote links remain after local files disappear. Neither action deletes remote data.
- Upload queue controls persist global/per-job pause intent across restart. Recovery resumes known sessions from immutable staged bytes; uncertain creation requires resolution rather than blindly making another upload. Clipboard failure after success offers another copy, without uploading again. Typed task profiles control after-capture/after-upload ordering; automatic upload is opt-in.

## Recording limits

Recording regions must fit one monitor. Default countdown is 3 seconds (0–60 configurable); recording cursor starts on. Pause/Resume excludes paused time from active duration.

| Format | FPS | Default active limit | Configurable maximum | Audio |
|---|---|---|---|---|
| GIF | 10 / 15 / 20 (default 15) | 60 seconds | 1–300 seconds | Always silent |
| MP4 (H.264) | 15 / 30 / 60 (default 30) | 10 minutes | 1–60 minutes | Off by default; system, microphone, or both mixed into one AAC track |

MP4 quality is Small/Balanced/High (default Balanced) or custom 0.5–100 Mbps. Output is Original or fit within 1280×720 / 1920×1080 without upscaling. Optional active-time stop stays within the maximum cap. Discard confirmation starts enabled. Recording uses Windows APIs/codecs; native codec/device behavior, locked desktop and real A/V timing remain unexercised in this delivery. No FFmpeg/driver installer is bundled.

## Local data and deletion

`AppPaths` resolves the existing **v1 layout** under the current user's Windows LocalAppData known folder:

| Location | Purpose |
|---|---|
| `%LOCALAPPDATA%\VectrCap\settings.json` | Validated versioned preferences and nonsecret credential references |
| `%LOCALAPPDATA%\VectrCap\history.db` (+ SQLite sidecars) | Durable jobs, receipts, workflows, history and pause/favorite state |
| `%LOCALAPPDATA%\VectrCap\spool\` | Immutable pending/retry sources, separate from visible archives |
| Windows **Pictures known folder** `\VectrCap\YYYY-MM\` | Default finalized screenshot/recording archive; Pictures may be redirected |

Default retention is **Always save**; **Keep pending/failed only** and **Never save automatically** are available. Explicit manual Save overrides automatic policy. Never-save still retains sources needed for accepted uploads. Capture source pixels remain in memory until an explicit output action.

Optional age cleanup is **disabled by default**. When enabled (1–3650 days), it can remove only eligible unchanged automatic archives with recorded ownership and proven persistent file identity. Manual saves, files needed by unfinished jobs, and modified/moved/replaced/reparse targets are retained/refused. Favorites affect browsing, not cleanup eligibility. Unsupported identity proof conservatively retains files. Changing the archive root does not move old files. Removing local settings/history or a token does not revoke server credentials or delete server files; manage those in Vectr. Prefer the app's explicit local-data actions while it is running.

Preferences JSON backup includes connection preferences and the selected archive path, but excludes tokens, history, live pause/favorite state, endpoint identities, installation data and diagnostics. Import previews and per-group selections apply through existing validation/transactions.

## Troubleshooting

Open **Settings → Troubleshooting**, select a destination, then use the explicit controls:

- **Refresh** collects only aggregate job counts and storage metadata, off the UI/controller thread. A shared 512-entry budget, eight directory levels and a 100 ms deadline between filesystem operations bound a scan; partial counts/errors are labeled. An individual OS/network filesystem call can take longer. Media contents, credentials and history row contents are not read. Capture/audio/encoder availability stays **not probed**; the page never opens streams or permission prompts.
- **Test connection** independently requests unauthenticated `GET /healthz` (bounded JSON `{"ok":true}`) and reuses the existing token-only `GET /api/uploader` client. A healthy service **does not verify a token**. Only successful uploader metadata validates uploader access; 404 stays unverified/manual-domain. **Cancel connection test** stops pending requests while retaining completed probe results. Leaving Settings also cancels pending tests. There is no `/api/me` call or test upload.
- **Preview diagnostic report** freezes versioned JSON before Save/Copy. Both actions use exactly the displayed bytes, at most 1 MiB. Refresh never silently regenerates an existing preview. Reports contain generic availability/counters/byte totals/stage/error codes, excluding paths, origins, destination/device/uploader names, links, tokens, raw errors/logs, settings/config bytes and history contents. No report import or automatic sending exists. Cancel/save/copy failures retain the preview and do not block capture.

For a missing token, configure the selected destination. For TLS/redirect failures, correct the server/certificate configuration. For a partial storage scan or full/unavailable archive, review local storage permissions/capacity and retention settings. Successful upload receipts remain available after clipboard or local archive failure.

## Developer build

Prerequisites: Windows 11 x64, Git, PowerShell, Rust/MSVC **1.98.1** with rustfmt/clippy and target `x86_64-pc-windows-msvc`, Visual Studio C++ desktop build tools and a Windows SDK. Bundled SQLite/WebP compile locally. No administrator-level installation is performed by repository scripts.

Provision explicitly (outside the build script):

```powershell
rustup toolchain install 1.98.1 --profile minimal --component rustfmt --component clippy --target x86_64-pc-windows-msvc
$env:RUSTUP_AUTO_INSTALL = '0'
rustc +stable --version  # must equal the rust-toolchain.toml pin: 1.98.1
cargo +stable fetch --locked --target x86_64-pc-windows-msvc
cargo +stable fmt --check
cargo +stable clippy --all-targets --offline -- -D warnings
cargo +stable test --all-targets --offline
cargo build --release --locked --target x86_64-pc-windows-msvc
./scripts/build-release.ps1
```

This project's verification commands use the installed `+stable` alias **only after checking it matches the exact pin**. Installing the named toolchain does not change that alias. CI provisions both explicitly and fails if stable has advanced beyond the pin; update the project's pin through a reviewed toolchain change. Auto-install is disabled for all verification/build commands.

`build-release.ps1` verifies the alias and target, builds with `--locked`, checks notices against the locked Windows dependency graph, and writes a fresh `target/packages/VectrCap-x64-…/` directory containing `VectrCap.exe`, README, `assets/THIRD-PARTY-NOTICES.md` and the Geist font license. Fonts are embedded in the binary. No global install, system-policy change or registry modification occurs. To update reviewed crate notices after a lockfile change, run `./scripts/generate-notices.ps1`; it uses cached sources and committed attributed supplements, not runtime network downloads.

[CI](.github/workflows/ci.yml) runs headless checks and packaging only. Interactive tests remain ignored; never interpret that as Windows host acceptance. The [design](docs/superpowers/specs/2026-11-13-vectrcap-design.md) and [implementation plan](docs/superpowers/plans/2026-11-13-vectrcap.md) define the release scope.
