pub mod windows;

use crate::{
    app::{destinations::CredentialKey, error::AppError},
    vectr::client::UploadToken,
};

pub trait CredentialStore: Send + Sync {
    fn read(&self, key: &CredentialKey) -> Result<UploadToken, AppError>;
    fn write(&self, key: &CredentialKey, token: &UploadToken) -> Result<(), AppError>;
    fn delete(&self, key: &CredentialKey) -> Result<(), AppError>;
}

pub struct CredentialClientFactory<S> {
    store: std::sync::Arc<S>,
    root: Option<reqwest::Certificate>,
}
impl<S: CredentialStore> CredentialClientFactory<S> {
    pub fn new(store: std::sync::Arc<S>) -> Self {
        Self { store, root: None }
    }
    pub fn with_root_certificate(store: std::sync::Arc<S>, root: reqwest::Certificate) -> Self {
        Self {
            store,
            root: Some(root),
        }
    }
}
impl<S: CredentialStore> crate::vectr::uploader::ClientFactory for CredentialClientFactory<S> {
    fn client(
        &self,
        destination: &crate::app::destinations::DestinationSnapshot,
    ) -> Result<crate::vectr::client::VectrClient, AppError> {
        let key = &destination.credential;
        if key.destination() != destination.id
            || key.origin() != &destination.origin
            || key.generation() == 0
            || destination.id.0.is_nil()
        {
            return Err(AppError::Credential("snapshot credential binding mismatch"));
        }
        let token = self.store.read(key)?;
        match &self.root {
            Some(root) => crate::vectr::client::VectrClient::with_root_certificate(
                destination.origin.clone(),
                token,
                root.clone(),
            ),
            None => crate::vectr::client::VectrClient::new(destination.origin.clone(), token),
        }
    }
}
