use super::*;
use crate::app::{
    cli::{LaunchRequest, LaunchRequestKind},
    destination_install::*,
    ingress::IngressAck,
};
use crate::workflow::runner::WorkflowContext;
#[derive(Clone)]
pub struct InstallView {
    pub request: Uuid,
    pub name: String,
    pub origin: String,
    pub state: InstallState,
    pub verification: Verification,
    pub uploader_name: Option<String>,
    pub can_delete_source: bool,
    pub install_allowed: bool,
}
#[derive(Clone)]
pub struct PendingSelectionView {
    pub request: Uuid,
    pub count: usize,
    pub names: Vec<String>,
    pub import: bool,
}
impl AppController {
    pub fn pending_selections(&self) -> Result<Vec<PendingSelectionView>, AppError> {
        let mut views = vec![];
        for pending in self.ingress.pending()? {
            let id = pending.request.request_id;
            if self.batches.contains_key(&id)
                || self
                    .install
                    .as_ref()
                    .is_some_and(|(request, _)| *request == id)
            {
                continue;
            }
            let (paths, import) = match pending.request.kind {
                LaunchRequestKind::UploadFiles { paths } => (paths, false),
                LaunchRequestKind::ImportDestination { path } => (vec![path], true),
            };
            let names = paths
                .iter()
                .take(3)
                .map(|p| {
                    std::path::Path::new(p).file_name().map_or_else(
                        || "selected file".into(),
                        |n| n.to_string_lossy().chars().take(255).collect(),
                    )
                })
                .collect();
            views.push(PendingSelectionView {
                request: id,
                count: paths.len(),
                names,
                import,
            });
        }
        Ok(views)
    }
    pub(super) fn cancel_pending_ingress(&mut self, id: Uuid) -> Result<Vec<AppEffect>, AppError> {
        if self.batches.contains_key(&id)
            || self
                .install
                .as_ref()
                .is_some_and(|(request, _)| *request == id)
        {
            return Err(AppError::InUse(
                "cancel active work in Uploads or the installation review",
            ));
        }
        self.ingress.complete(id)?;
        let mut effects = vec![];
        if let Err(error) = self.update_active() {
            effects.push(AppEffect::Error(error));
        }
        Ok(effects)
    }
    pub fn pending_ingress_count(&self) -> Result<usize, AppError> {
        Ok(self.ingress.pending()?.len())
    }
    pub fn install_view(&self) -> Option<InstallView> {
        let (request, session) = self.install.as_ref()?;
        let (name, origin) = session.display();
        Some(InstallView {
            request: *request,
            name: name.into(),
            origin: origin.as_str().into(),
            state: session.state(),
            verification: session.verification(),
            uploader_name: session.uploader_name().map(str::to_owned),
            can_delete_source: session.can_delete_source(),
            install_allowed: session.install_allowed(),
        })
    }
    pub fn accept_launch(
        &mut self,
        request: LaunchRequest,
    ) -> Result<(IngressAck, Vec<AppEffect>), AppError> {
        self.accept_launch_for(request, DestinationSelection::Inherit, None)
    }
    pub(super) fn accept_launch_for(
        &mut self,
        request: LaunchRequest,
        selection: DestinationSelection,
        action: Option<super::super::shortcuts::ShortcutAction>,
    ) -> Result<(IngressAck, Vec<AppEffect>), AppError> {
        self.manager.ensure_not_corrupted()?;
        let mut context = self.task_context(ArtifactKind::File, action, selection);
        context.id = request.request_id;
        context.batch_id = Some(request.request_id);
        let ack =
            self.ingress
                .accept_context(request, context.destination.clone(), Some(&context))?;
        // The durable primary result survives later staging/prompt failures.
        let effects = match self.resume_ingress() {
            Ok(effects) => effects,
            Err(error) => vec![AppEffect::Error(error)],
        };
        Ok((ack, effects))
    }
    pub fn resume_ingress(&mut self) -> Result<Vec<AppEffect>, AppError> {
        let mut effects = vec![];
        for pending in self.ingress.pending()? {
            let id = pending.request.request_id;
            match pending.request.kind {
                LaunchRequestKind::ImportDestination { path } => {
                    if self.install.is_some() {
                        continue;
                    }
                    match DestinationInstallSession::read(std::path::Path::new(&path)) {
                        Ok(session) => {
                            self.install = Some((id, session));
                            self.window = Some(Window::Settings);
                            effects.push(AppEffect::Window(self.window));
                        }
                        Err(error) => {
                            effects.push(AppEffect::Error(error));
                            if let Err(error) = self.ingress.complete(id) {
                                effects.push(AppEffect::Error(error));
                            }
                        }
                    }
                }
                LaunchRequestKind::UploadFiles { paths } => {
                    if self.batches.contains_key(&id) {
                        self.maybe_flush_batch(id, &mut effects);
                        continue;
                    }
                    let mut context = pending.context.unwrap_or_else(|| {
                        self.task_context(
                            ArtifactKind::File,
                            Some(super::super::shortcuts::ShortcutAction::UploadFile),
                            DestinationSelection::Inherit,
                        )
                    });
                    context.id = id;
                    context.batch_id = Some(id);
                    if context.destination.is_none() {
                        match context.resolve_ingress_setup(&self.settings().destinations) {
                            Ok(()) => {
                                if let Err(error) = self.ingress.save_context(id, &context) {
                                    effects.push(AppEffect::Error(error));
                                    continue;
                                }
                            }
                            Err(AppError::NeedsSetup(_)) => {
                                self.window = Some(Window::Settings);
                                effects.push(AppEffect::Window(self.window));
                                continue;
                            }
                            Err(error) => {
                                effects.push(AppEffect::Error(error));
                                continue;
                            }
                        }
                    }
                    let snapshot = context
                        .destination
                        .clone()
                        .ok_or(AppError::NeedsSetup("selected destination needs setup"))?;
                    if let Err(error) = self.store.read(&snapshot.credential) {
                        effects.push(AppEffect::Error(error));
                        self.window = Some(Window::Settings);
                        effects.push(AppEffect::Window(self.window));
                        continue;
                    }
                    if let Err(error) = self.start_ingress_batch(id, paths, context, &mut effects) {
                        effects.push(AppEffect::Error(error));
                    }
                }
            }
        }
        if let Err(error) = self.update_active() {
            effects.push(AppEffect::Error(error));
        }
        Ok(effects)
    }
    fn start_ingress_batch(
        &mut self,
        batch: Uuid,
        paths: Vec<std::ffi::OsString>,
        context: WorkflowContext,
        out: &mut Vec<AppEffect>,
    ) -> Result<(), AppError> {
        if self.batches.len() >= 32 {
            return Err(AppError::InUse("too many concurrent selections"));
        }
        let snapshot = context
            .destination
            .clone()
            .ok_or(AppError::NeedsSetup("batch destination needs setup"))?;
        let count = paths.len();
        let copy = context
            .effective
            .profile
            .after_upload
            .contains(&crate::workflow::profile::UploadTask::CopyUrl);
        let mut state = FileBatch {
            task_context: context,
            staging_remaining: count,
            staging_done: vec![false; count],
            jobs_remaining: 0,
            completions: vec![None; count],
            results: super::super::batches::UploadBatch::new(count, copy)?,
        };
        // Recover the exact workflow IDs. A crash between checkpoint and inbox
        // settlement cannot create another upload, resolve a new destination or
        // substitute new source bytes for an already accepted member.
        let mut todo = vec![];
        let mut recovered = vec![];
        for (index, path) in paths.into_iter().enumerate() {
            let task = member_id(batch, index);
            if let Some(runner) = self.history.task_workflow(task)? {
                state.staging_done[index] = true;
                state.staging_remaining -= 1;
                if runner.complete() && runner.context().upload_job.is_none() {
                    state
                        .results
                        .resolve(index, super::super::batches::BatchOutcome::Cancelled)?;
                    continue;
                }
                state.jobs_remaining += 1;
                recovered.push((task, index, runner));
            } else if self.ingress.rejected(batch, index)? {
                state.staging_done[index] = true;
                state.staging_remaining -= 1;
                state
                    .results
                    .resolve(index, super::super::batches::BatchOutcome::Failed)?;
            } else {
                todo.push((index, std::path::PathBuf::from(path)));
            }
        }
        if state.staging_remaining > 0 {
            self.workflows.insert(batch, snapshot);
        }
        self.batches.insert(batch, state);
        if let Err(error) = self.update_active() {
            self.workflows.remove(&batch);
            self.batches.remove(&batch);
            return Err(error);
        }
        for (task, index, runner) in recovered {
            if let Some(job) = runner.context().upload_job {
                self.job_batch.insert(job, (batch, index));
            } else {
                self.task_batches.insert(task, (batch, index));
            }
            self.task_runners.insert(task, runner);
        }
        out.push(AppEffect::WorkflowAccepted(batch));
        // Preserve original indices when only a subset needs staging on recovery.
        if todo.len() == count {
            out.push(AppEffect::StageFiles {
                batch,
                paths: todo.into_iter().map(|(_, path)| path).collect(),
            });
        } else {
            for (index, path) in todo {
                out.push(AppEffect::StageIngressFile { batch, index, path });
            }
        }
        let jobs: Vec<_> = self
            .job_batch
            .iter()
            .filter_map(|(job, (b, _))| (*b == batch).then_some(*job))
            .collect();
        for job in jobs {
            out.extend(
                self.on_event(AppEvent::Queue(crate::vectr::queue::QueueEffect::Changed(
                    job,
                ))),
            );
        }
        self.maybe_flush_batch(batch, out);
        Ok(())
    }
    pub(super) fn install_command(
        &mut self,
        request: Uuid,
        command: InstallCommand,
    ) -> Result<Vec<AppEffect>, AppError> {
        let (_, session) = self
            .install
            .as_mut()
            .filter(|(id, _)| *id == request)
            .ok_or(AppError::InvalidInput("installation review changed"))?;
        match command {
            InstallCommand::Verify => {
                session.consent()?;
                let client = match session.verification_client() {
                    Ok(client) => client,
                    Err(error) => {
                        // A client-construction failure must leave a retryable
                        // review, never a Verifying state with no worker.
                        let _ = session.verification_result(Err(error.clone()));
                        return Err(error);
                    }
                };
                Ok(vec![AppEffect::VerifyInstall { request, client }])
            }
            InstallCommand::AllowUnverified => {
                session.allow_unverified()?;
                Ok(vec![])
            }
            InstallCommand::Install(choice) => {
                session.install(&mut self.manager, choice)?;
                let mut effects = vec![AppEffect::SettingsSaved(self.settings().clone())];
                if let Err(error) = self.ingress.complete(request) {
                    effects.push(AppEffect::Error(error));
                }
                Ok(effects)
            }
            InstallCommand::DeleteSource => {
                session.delete_source()?;
                Ok(vec![])
            }
            InstallCommand::Close => {
                // Complete before dropping a pending review; a checkpoint failure leaves
                // the existing session visible and never replays a user confirmation.
                self.ingress.complete(request)?;
                if let Some((_, mut session)) = self.install.take() {
                    session.cancel();
                }
                let mut effects = vec![AppEffect::CancelInstallVerify];
                match self.resume_ingress() {
                    Ok(next) => effects.extend(next),
                    Err(error) => effects.push(AppEffect::Error(error)),
                }
                Ok(effects)
            }
        }
    }
}
pub enum InstallCommand {
    Verify,
    AllowUnverified,
    Install(InstallChoice),
    DeleteSource,
    Close,
}
pub(super) fn member_id(batch: Uuid, index: usize) -> Uuid {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(b"VectrCap ingress member v1");
    hash.update(batch.as_bytes());
    hash.update((index as u32).to_le_bytes());
    let digest = hash.finalize();
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest[..16]);
    Uuid::from_bytes(bytes)
}
