//! Native multi-select "Upload file" picker. `Show` is modal and blocking by
//! design, so it always runs on its own dedicated OS thread, never on the
//! egui/UI thread or the controller's Tokio runtime. A cancelled dialog is a
//! pure no-op: nothing is staged, no command is submitted.
#![allow(unsafe_code)]
use crate::app::{desktop::DesktopClient, error::AppError, events::AppCommand, notifications};
use std::path::PathBuf;
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

/// Opens the native picker on a dedicated thread and, on a non-empty
/// selection, hands the paths to the controller as one accepted batch.
/// Never blocks the calling (egui/UI or tray) thread. Bounding the batch
/// here, before anything is touched, keeps an oversized selection from
/// reaching the controller at all.
pub fn spawn_pick_and_upload(client: DesktopClient) {
    std::thread::spawn(move || match pick_existing_files() {
        Ok(None) => {}
        Ok(Some(paths)) => {
            if paths.len() > notifications::MAX_BATCH_ITEMS {
                client.report_error(AppError::InvalidInput("select at most 100 files to upload"));
                return;
            }
            if let Err(submit_error) = client.submit(AppCommand::AcceptFileSelection(paths)) {
                client.report_error(submit_error);
            }
        }
        Err(error) => client.report_error(error),
    });
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
