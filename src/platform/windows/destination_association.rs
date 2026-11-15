use super::explorer::*;
use crate::app::error::AppError;
use std::path::PathBuf;
pub const EXTENSION_KEY: &str = r"Software\Classes\.vectrcap";
pub const PROGID_KEY: &str = r"Software\Classes\VectrCap.Destination.1";
pub struct DestinationAssociation<'a> {
    registry: &'a dyn RegistryBackend,
    executable: PathBuf,
}
impl<'a> DestinationAssociation<'a> {
    pub fn new(registry: &'a dyn RegistryBackend, executable: PathBuf) -> Self {
        Self {
            registry,
            executable,
        }
    }
    fn templates(&self) -> Result<Vec<(String, RegistryTree)>, AppError> {
        let quoted = quote_executable(&self.executable)?;
        let exe = self
            .executable
            .to_str()
            .ok_or(AppError::InvalidInput("invalid executable path"))?;
        let mut progid = owned_tree(exe);
        progid.set("", "", "VectrCap destination configuration");
        progid.set(
            "shell\\open\\command",
            "",
            &format!("{quoted} --import-destination -- \"%1\""),
        );
        let mut trees = vec![(PROGID_KEY.into(), progid)];
        // Never replace an existing extension association or inspect/write UserChoice.
        if self
            .registry
            .read_tree(EXTENSION_KEY)?
            .as_ref()
            .is_none_or(RegistryTree::owned_by_app)
        {
            let mut extension = owned_tree(exe);
            extension.set("", "", "VectrCap.Destination.1");
            trees.push((EXTENSION_KEY.into(), extension));
        }
        Ok(trees)
    }
    pub fn enable(&self) -> Result<(), AppError> {
        if !self.executable.is_file() {
            return Err(AppError::InvalidInput(
                "install a real executable before enabling association",
            ));
        }
        registration_apply(self.registry, &self.templates()?, true)
    }
    pub fn disable(&self) -> Result<(), AppError> {
        registration_apply(self.registry, &self.templates()?, false)
    }
    pub fn status(&self) -> Result<RegistrationStatus, AppError> {
        registration_status(self.registry, &self.templates()?)
    }
}
