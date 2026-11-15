//! Native multi-select "Upload file" picker. `Show` is modal and blocking by
//! design, so it always runs on its own dedicated OS thread, never on the
//! egui/UI thread or the controller's Tokio runtime. A cancelled dialog is a
//! pure no-op: nothing is staged, no command is submitted.
#![allow(unsafe_code)]
use crate::app::{
    desktop::DesktopClient,
    error::AppError,
    events::{AppCommand, Lifecycle},
    notifications,
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use windows::Win32::{
    System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoTaskMemFree, CoUninitialize,
    },
    UI::Shell::{
        FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST,
        IFileOpenDialog, IShellItem, SIGDN_FILESYSPATH,
    },
};

/// `CLSID_FileOpenDialog`. This build of the `windows` crate does not emit a
/// class marker for it, so the well-known CLSID is spelled out directly, in
/// the same literal form `windows`-generated interface IIDs use.
const FILE_OPEN_DIALOG: windows::core::GUID =
    windows::core::GUID::from_u128(0xDC1C5A9C_E88A_4DDE_A5A1_60F82A20AEF7);
/// `HRESULT` for `ERROR_CANCELLED` (the user closed or cancelled the dialog).
const ERROR_CANCELLED: i32 = 0x800704C7u32 as i32;

fn error() -> AppError {
    AppError::Persistence("native file picker unavailable")
}

struct Com;
impl Drop for Com {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

/// Shows the native "Upload file" picker on the calling thread and blocks
/// until it closes. `Ok(None)` means the user cancelled.
fn pick_existing_files() -> Result<Option<Vec<PathBuf>>, AppError> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(|_| error())?;
        let _com = Com;
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FILE_OPEN_DIALOG, None, CLSCTX_INPROC_SERVER).map_err(|_| error())?;
        let options = dialog.GetOptions().map_err(|_| error())?;
        dialog
            .SetOptions(
                options
                    | FOS_ALLOWMULTISELECT
                    | FOS_FILEMUSTEXIST
                    | FOS_PATHMUSTEXIST
                    | FOS_FORCEFILESYSTEM,
            )
            .map_err(|_| error())?;
        match dialog.Show(None) {
            Ok(()) => {}
            Err(e) if e.code().0 == ERROR_CANCELLED => return Ok(None),
            Err(_) => return Err(error()),
        }
        let results = dialog.GetResults().map_err(|_| error())?;
        let count = results.GetCount().map_err(|_| error())?;
        // Bounded BEFORE any allocation or per-item shell call: a pathological
        // (or corrupted) count must never size a `Vec` or walk `GetItemAt` for
        // more items than the app will ever accept.
        if count as usize > notifications::MAX_BATCH_ITEMS {
            return Err(AppError::InvalidInput("select at most 100 files to upload"));
        }
        let mut paths = Vec::with_capacity(count as usize);
        for index in 0..count {
            let item: IShellItem = results.GetItemAt(index).map_err(|_| error())?;
            let name = item
                .GetDisplayName(SIGDN_FILESYSPATH)
                .map_err(|_| error())?;
            let text = name.to_string().map_err(|_| error());
            CoTaskMemFree(Some(name.0.cast()));
            paths.push(PathBuf::from(text?));
        }
        if paths.is_empty() {
            return Ok(None);
        }
        Ok(Some(paths))
    }
}

/// A native Save chooser selects a path only. Publication remains with the
/// validated finish transaction and always refuses an existing destination.
fn pick_image_save(extension: &str) -> Result<Option<PathBuf>, AppError> {
    use windows::{
        Win32::UI::Shell::{FileSaveDialog, IFileSaveDialog},
        core::PCWSTR,
    };
    let extension: Vec<u16> = extension.encode_utf16().chain([0]).collect();
    let name: Vec<u16> = "VectrCap".encode_utf16().chain([0]).collect();
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(|_| error())?;
        let _com = Com;
        let dialog: IFileSaveDialog =
            CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).map_err(|_| error())?;
        dialog
            .SetOptions(
                dialog.GetOptions().map_err(|_| error())? | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST,
            )
            .map_err(|_| error())?;
        dialog
            .SetDefaultExtension(PCWSTR(extension.as_ptr()))
            .map_err(|_| error())?;
        dialog
            .SetFileName(PCWSTR(name.as_ptr()))
            .map_err(|_| error())?;
        match dialog.Show(None) {
            Ok(()) => {}
            Err(e) if e.code().0 == ERROR_CANCELLED => return Ok(None),
            Err(_) => return Err(error()),
        }
        let item = dialog.GetResult().map_err(|_| error())?;
        let name = item
            .GetDisplayName(SIGDN_FILESYSPATH)
            .map_err(|_| error())?;
        let text = name.to_string().map_err(|_| error());
        CoTaskMemFree(Some(name.0.cast()));
        Ok(Some(PathBuf::from(text?)))
    }
}

pub fn spawn_capture_save(
    client: DesktopClient,
    surface: Arc<std::sync::Mutex<crate::app::controller::CaptureSurface>>,
    finish: crate::ui::image_export::ImageFinish,
) {
    spawn_capture_dialog(
        client,
        surface,
        move |client, session| match pick_image_save(finish.prepared.extension())? {
            Some(path) => client.submit(AppCommand::FinishCapture {
                session,
                finish,
                save_path: Some(path),
            }),
            None => client.submit(AppCommand::ResumeCapture(session)),
        },
    );
}

pub fn spawn_capture_archive_folder(
    client: DesktopClient,
    surface: Arc<std::sync::Mutex<crate::app::controller::CaptureSurface>>,
    mut policy: crate::persistence::settings::ArchivePolicy,
    revision: u64,
) {
    spawn_capture_dialog(client, surface, move |client, _| unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(|_| error())?;
        let _com = Com;
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FILE_OPEN_DIALOG, None, CLSCTX_INPROC_SERVER).map_err(|_| error())?;
        dialog
            .SetOptions(
                FOS_FORCEFILESYSTEM
                    | FOS_PATHMUSTEXIST
                    | windows::Win32::UI::Shell::FOS_PICKFOLDERS,
            )
            .map_err(|_| error())?;
        match dialog.Show(None) {
            Ok(()) => {}
            Err(e) if e.code().0 == ERROR_CANCELLED => return Ok(()),
            Err(_) => return Err(error()),
        }
        let item = dialog.GetResult().map_err(|_| error())?;
        let name = item
            .GetDisplayName(SIGDN_FILESYSPATH)
            .map_err(|_| error())?;
        let text = name.to_string().map_err(|_| error());
        CoTaskMemFree(Some(name.0.cast()));
        policy.archive_root = Some(PathBuf::from(text?));
        policy.validate()?;
        client.submit(AppCommand::SetArchivePolicy { policy, revision })
    });
}

fn spawn_capture_dialog(
    client: DesktopClient,
    surface: Arc<std::sync::Mutex<crate::app::controller::CaptureSurface>>,
    picker: impl FnOnce(&DesktopClient, uuid::Uuid) -> Result<(), AppError> + Send + 'static,
) {
    let Ok(mut state) = surface.lock() else {
        client.report_error(error());
        return;
    };
    if state.dialog_open {
        return;
    }
    state.dialog_open = true;
    let session = state.session;
    drop(state);
    let report = client.clone();
    let reset = surface.clone();
    if std::thread::Builder::new()
        .name("vectrcap-capture-picker".into())
        .spawn(move || {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| picker(&client, session)))
                    .unwrap_or(Err(AppError::Persistence("capture picker worker failed")));
            if let Ok(mut state) = surface.lock() {
                state.dialog_open = false;
                if let Err(error) = &result {
                    state.working = false;
                    state.overlay.finish_failed(error.clone());
                }
            }
            if let Err(error) = result {
                client.report_error(error);
            }
            // Even a cancelled picker must wake the hidden capture viewport.
            client.wake();
        })
        .is_err()
    {
        if let Ok(mut state) = reset.lock() {
            state.dialog_open = false;
            state.working = false;
            state.overlay.finish_failed(error());
        }
        report.report_error(error());
    }
}

/// Single-flight gate: at most one native picker dialog may be open at a
/// time. Acquiring it returns a guard that releases on drop, including on a
/// panic inside the picker thread or a failed thread launch — so a failure
/// can never leave the gate stuck busy forever.
#[derive(Clone, Default)]
pub struct PickerGate(Arc<AtomicBool>);
struct PickerGuard(Arc<AtomicBool>);
impl Drop for PickerGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
impl PickerGate {
    fn try_acquire(&self) -> Option<PickerGuard> {
        self.0
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| PickerGuard(self.0.clone()))
    }
}

/// Opens the native picker on a dedicated thread and, on a non-empty
/// selection, hands the paths to the controller as one accepted batch.
/// Never blocks the calling (egui/UI or tray) thread.
pub fn spawn_pick_and_upload_for(
    client: DesktopClient,
    gate: PickerGate,
    selection: crate::app::destinations::DestinationSelection,
) {
    spawn_pick_with_selection(client, gate, selection, pick_existing_files);
}
pub fn spawn_pick_and_upload(client: DesktopClient, gate: PickerGate) {
    spawn_pick_with(client, gate, pick_existing_files);
}

/// Single-image picker shares the app's native-dialog gate with Upload file.
pub fn spawn_pick_pin_file(client: DesktopClient, gate: PickerGate) {
    if client.snapshot().lifecycle != Lifecycle::Running {
        return;
    }
    let Some(guard) = gate.try_acquire() else {
        client.report_error(AppError::InUse("a file picker is already open"));
        return;
    };
    let report = client.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("vectrcap-pin-picker".into())
        .spawn(move || {
            let _guard = guard;
            let result = pick_pin_file();
            if client.snapshot().lifecycle != Lifecycle::Running {
                return;
            }
            match result {
                Ok(Some(path)) => {
                    if let Err(error) = client.submit(AppCommand::LoadPinFile(path)) {
                        client.report_error(error);
                    }
                }
                Ok(None) => {}
                Err(error) => client.report_error(error),
            }
        })
    {
        report.report_error(AppError::io("starting pin file picker", &error));
    }
}
fn pick_pin_file() -> Result<Option<PathBuf>, AppError> {
    use windows::{Win32::UI::Shell::Common::COMDLG_FILTERSPEC, core::w};
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(|_| error())?;
        let _com = Com;
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FILE_OPEN_DIALOG, None, CLSCTX_INPROC_SERVER).map_err(|_| error())?;
        dialog
            .SetOptions(FOS_FORCEFILESYSTEM | FOS_FILEMUSTEXIST | FOS_PATHMUSTEXIST)
            .map_err(|_| error())?;
        dialog
            .SetTitle(w!("Pin image (GIF: first frame)"))
            .map_err(|_| error())?;
        dialog
            .SetFileTypes(&[COMDLG_FILTERSPEC {
                pszName: w!("Raster images (GIF: first frame)"),
                pszSpec: w!("*.png;*.jpg;*.jpeg;*.gif;*.bmp;*.webp"),
            }])
            .map_err(|_| error())?;
        match dialog.Show(None) {
            Ok(()) => {}
            Err(e) if e.code().0 == ERROR_CANCELLED => return Ok(None),
            Err(_) => return Err(error()),
        }
        let item = dialog.GetResult().map_err(|_| error())?;
        let name = item
            .GetDisplayName(SIGDN_FILESYSPATH)
            .map_err(|_| error())?;
        let text = name.to_string().map_err(|_| error());
        CoTaskMemFree(Some(name.0.cast()));
        Ok(Some(PathBuf::from(text?)))
    }
}

/// `spawn_pick_and_upload`'s real logic, parameterized over the picker call
/// itself so lifecycle/gating/bounding can be exercised deterministically in
/// tests without a real native dialog.
pub fn spawn_pick_with<P>(client: DesktopClient, gate: PickerGate, picker: P)
where
    P: FnOnce() -> Result<Option<Vec<PathBuf>>, AppError> + Send + 'static,
{
    spawn_pick_with_selection(
        client,
        gate,
        crate::app::destinations::DestinationSelection::Inherit,
        picker,
    );
}
pub fn spawn_pick_with_selection<P>(
    client: DesktopClient,
    gate: PickerGate,
    selection: crate::app::destinations::DestinationSelection,
    picker: P,
) where
    P: FnOnce() -> Result<Option<Vec<PathBuf>>, AppError> + Send + 'static,
{
    // Shutting down: never open a dialog, never touch a file.
    if client.snapshot().lifecycle != Lifecycle::Running {
        return;
    }
    let view = client.snapshot();
    if view.capture_acquiring.is_some() || view.capture_surface.is_some() {
        client.report_error(AppError::InUse(
            "finish or cancel the capture before choosing upload files",
        ));
        return;
    }
    let Some(guard) = gate.try_acquire() else {
        client.report_error(AppError::InUse(
            "a file picker is already open; finish or cancel it first",
        ));
        return;
    };
    let report_client = client.clone();
    let spawned = std::thread::Builder::new()
        .name("vectrcap-file-picker".into())
        .spawn(move || {
            let _guard = guard;
            let result = picker();
            // The dialog may have resolved after shutdown began; a late
            // selection must never be accepted, and nothing it names has
            // been touched (the picker only resolves display paths).
            if client.snapshot().lifecycle != Lifecycle::Running {
                return;
            }
            match result {
                Ok(None) => {}
                Ok(Some(paths)) => {
                    if let Err(submit_error) =
                        client.submit(AppCommand::AcceptFileSelectionFor { paths, selection })
                    {
                        client.report_error(submit_error);
                    }
                }
                Err(error) => client.report_error(error),
            }
        });
    // A failed launch releases the gate itself (the never-run closure, and
    // the guard it captured, are dropped here), but the user still needs to
    // hear about it.
    if spawned.is_err() {
        report_client.report_error(error());
    }
}

/// Dedicated-thread archive-folder selection. Revision checks reject stale dialogs.
pub fn spawn_pick_archive_folder(
    client: DesktopClient,
    mut policy: crate::persistence::settings::ArchivePolicy,
    revision: u64,
) {
    let report = client.clone();
    if std::thread::Builder::new()
        .name("vectrcap-archive-picker".into())
        .spawn(move || {
            let result = (|| -> Result<(), AppError> {
                unsafe {
                    CoInitializeEx(None, COINIT_APARTMENTTHREADED)
                        .ok()
                        .map_err(|_| error())?;
                    let _com = Com;
                    let dialog: IFileOpenDialog =
                        CoCreateInstance(&FILE_OPEN_DIALOG, None, CLSCTX_INPROC_SERVER)
                            .map_err(|_| error())?;
                    dialog
                        .SetOptions(
                            FOS_FORCEFILESYSTEM
                                | FOS_PATHMUSTEXIST
                                | windows::Win32::UI::Shell::FOS_PICKFOLDERS,
                        )
                        .map_err(|_| error())?;
                    match dialog.Show(None) {
                        Ok(()) => {}
                        Err(e) if e.code().0 == ERROR_CANCELLED => return Ok(()),
                        Err(_) => return Err(error()),
                    }
                    let item = dialog.GetResult().map_err(|_| error())?;
                    let name = item
                        .GetDisplayName(SIGDN_FILESYSPATH)
                        .map_err(|_| error())?;
                    let text = name.to_string().map_err(|_| error());
                    CoTaskMemFree(Some(name.0.cast()));
                    policy.archive_root = Some(PathBuf::from(text?));
                    policy.validate()?;
                    client.submit(AppCommand::SetArchivePolicy { policy, revision })
                }
            })();
            if let Err(error) = result {
                client.report_error(error);
            }
        })
        .is_err()
    {
        report.report_error(error());
    }
}

pub fn spawn_task_save(client: DesktopClient, gate: PickerGate, id: uuid::Uuid, name: String) {
    let Some(guard) = gate.try_acquire() else {
        client.report_error(AppError::InUse("a file picker is already open"));
        return;
    };
    let report = client.clone();
    if let Err(e) = std::thread::Builder::new()
        .name("vectrcap-task-save".into())
        .spawn(move || {
            let _guard = guard;
            let extension = std::path::Path::new(&name)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("png");
            let result = pick_image_save(extension).and_then(|path| match path {
                Some(path) => client.submit(AppCommand::Task {
                    id,
                    command: crate::workflow::runner::TaskCommand::SaveAs(path),
                }),
                None => Ok(()),
            });
            if let Err(e) = result {
                client.report_error(e);
            }
        })
    {
        report.report_error(AppError::io("starting task Save as picker", &e));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "opens a real native dialog; requires a human to pick files"]
    fn manual_picker_returns_exactly_the_selected_synthetic_files() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("vectrcap-smoke-a.txt");
        let b = dir.path().join("vectrcap-smoke-b.txt");
        std::fs::write(&a, b"synthetic smoke file A").unwrap();
        std::fs::write(&b, b"synthetic smoke file B").unwrap();
        eprintln!(
            "MANUAL STEP: in the dialog, navigate to {} and select BOTH \
             vectrcap-smoke-a.txt and vectrcap-smoke-b.txt, then confirm.",
            dir.path().display()
        );
        let mut picked = pick_existing_files()
            .unwrap()
            .expect("the dialog must not be cancelled for this manual check");
        picked.sort();
        let mut expected = vec![a, b];
        expected.sort();
        assert_eq!(picked, expected);
    }

    #[test]
    #[ignore = "opens a real native dialog; requires a human to cancel it"]
    fn manual_cancelled_picker_is_a_no_op() {
        eprintln!("MANUAL STEP: cancel or close the dialog without selecting a file.");
        assert_eq!(pick_existing_files().unwrap(), None);
    }
}
