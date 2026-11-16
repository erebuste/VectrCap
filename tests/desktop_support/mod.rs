#![allow(dead_code)]
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use vectrcap::{
    app::{
        desktop::{DesktopRuntime, DesktopServices},
        error::AppError,
    },
    platform::{
        CredentialStore,
        windows::{clipboard::ClipboardWriter, shell_actions::ShellActions},
    },
    vectr::uploader::ClientFactory,
};
#[derive(Default)]
pub struct Clipboard(pub Mutex<Vec<String>>);
impl ClipboardWriter for Clipboard {
    fn write_text(&self, text: &str) -> Result<(), AppError> {
        self.0.lock().unwrap().push(text.into());
        Ok(())
    }
}
pub struct Shell;
impl ShellActions for Shell {
    fn open_path(&self, _: &Path) -> Result<(), AppError> {
        Ok(())
    }
    fn open_url(&self, _: &str) -> Result<(), AppError> {
        Ok(())
    }
    fn reveal_path(&self, _: &Path) -> Result<(), AppError> {
        Ok(())
    }
}
pub fn spawn(
    root: PathBuf,
    credentials: Arc<dyn CredentialStore>,
    clients: Arc<dyn ClientFactory>,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Result<DesktopRuntime, AppError> {
    spawn_recording(root, credentials, clients, wake).map(|(runtime, _)| runtime)
}
pub fn spawn_recording(
    root: PathBuf,
    credentials: Arc<dyn CredentialStore>,
    clients: Arc<dyn ClientFactory>,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Result<(DesktopRuntime, Arc<Clipboard>), AppError> {
    let clipboard = Arc::new(Clipboard::default());
    let runtime = DesktopRuntime::spawn_with_services(
        root,
        DesktopServices {
            credentials,
            clients,
            shell: Arc::new(Shell),
            clipboard: clipboard.clone(),
        },
        wake,
    )?;
    Ok((runtime, clipboard))
}
