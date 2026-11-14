use crate::{
    app::{destination_manager::DestinationEdit, destinations::DestinationId, error::AppError},
    persistence::settings::Settings,
    vectr::uploader_info::UploaderInfoSupport,
};
use uuid::Uuid;
use zeroize::Zeroizing;
pub struct ConnectionDraft {
    pub id: Option<DestinationId>,
    pub revision: u64,
    pub name: String,
    pub origin: String,
    pub token: Zeroizing<String>,
    pub domain: Option<String>,
    pub make_default: bool,
    pub pending_test: Option<Uuid>,
    pub metadata: Option<Result<UploaderInfoSupport, AppError>>,
}
impl ConnectionDraft {
    pub fn new(settings: &Settings, id: Option<DestinationId>) -> Self {
        let destination = id.and_then(|id| settings.destinations.get(id));
        Self {
            id,
            revision: settings.revision,
            name: destination.map(|d| d.name.clone()).unwrap_or_default(),
            origin: destination
                .map(|d| d.origin.as_str().to_owned())
                .unwrap_or_default(),
            token: Zeroizing::new(String::new()),
            domain: destination.and_then(|d| {
                d.link_domain_override
                    .as_ref()
                    .map(|d| d.as_str().to_owned())
            }),
            make_default: id.is_some() && settings.destinations.default_id() == id,
            pending_test: None,
            metadata: None,
        }
    }
    pub fn invalidate_test(&mut self) {
        self.pending_test = None;
        self.metadata = None;
    }
    pub fn accept_metadata(
        &mut self,
        request: Uuid,
        result: Result<UploaderInfoSupport, AppError>,
        current_revision: u64,
    ) {
        if self.revision != current_revision {
            self.invalidate_test();
            return;
        }
        if self.pending_test == Some(request) {
            self.pending_test = None;
            self.metadata = Some(result);
        }
    }
    pub fn prepare_save(&mut self, settings: &Settings) -> Result<DestinationEdit, AppError> {
        use crate::{
            persistence::settings::{LinkOrigin, ServerOrigin},
            vectr::{client::UploadToken, uploader_info::DomainChoice},
        };
        if settings.revision != self.revision {
            return Err(AppError::InvalidInput(
                "settings changed; cancel and reopen this connection",
            ));
        }
        let origin = ServerOrigin::parse(&self.origin)?;
        let domain = self
            .domain
            .as_deref()
            .filter(|d| !d.is_empty())
            .map(LinkOrigin::parse)
            .transpose()?;
        let old = self.id.and_then(|id| settings.destinations.get(id));
        let preserved = old.is_some_and(|d| d.origin == origin && d.link_domain_override == domain);
        if domain.is_some() && !preserved {
            let allowed = match &self.metadata {
                Some(Ok(UploaderInfoSupport::Unsupported404)) => true,
                Some(Ok(UploaderInfoSupport::Available(info))) => info.domains.iter().any(
                    |choice| matches!(choice, DomainChoice::Https(d) if Some(d) == domain.as_ref()),
                ),
                _ => false,
            };
            if !allowed {
                return Err(AppError::InvalidInput(
                    "choose a verified domain, or use uploader default; manual entry requires HTTP 404",
                ));
            }
        }
        if self.token.is_empty()
            && old.is_none_or(|d| d.origin != origin || d.credential_ref.is_none())
        {
            return Err(AppError::NeedsSetup(
                "enter an upload token for this origin",
            ));
        }
        let mut destinations = settings.destinations.clone();
        let id = match self.id {
            Some(id) => {
                destinations.rename(id, &self.name)?;
                destinations.change_origin(id, origin, &Default::default())?;
                destinations.set_link_domain(id, domain)?;
                id
            }
            None => destinations.add(&self.name, origin, domain)?,
        };
        if self.make_default {
            destinations.set_default(id)?;
        }
        let tokens = if self.token.is_empty() {
            vec![]
        } else {
            // Validate the borrowed field in place; only take (and thus
            // blank) it once we know construction will succeed, so an
            // invalid paste (e.g. trailing whitespace) leaves the field
            // intact for the user to see and correct.
            UploadToken::validate(&self.token)?;
            let token = UploadToken::new(std::mem::take(&mut *self.token))?;
            crate::platform::windows::credentials::validate_token(&token)?;
            vec![(id, token)]
        };
        Ok(DestinationEdit {
            destinations,
            tokens,
        })
    }
}
