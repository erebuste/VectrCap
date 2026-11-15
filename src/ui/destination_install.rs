use crate::app::{
    controller::ingress::{InstallCommand, InstallView},
    destination_install::{InstallChoice, InstallState, Verification},
    events::AppCommand,
};
use eframe::egui;
#[derive(Default)]
pub struct InstallReview {
    request: Option<uuid::Uuid>,
    name: String,
    set_default: bool,
    confirm_replace: Option<crate::app::destinations::DestinationId>,
    consent: bool,
    confirm_delete: bool,
}
impl InstallReview {
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        view: Option<&InstallView>,
        settings: Option<&crate::persistence::settings::Settings>,
    ) -> Vec<AppCommand> {
        let Some(view) = view else {
            self.request = None;
            return vec![];
        };
        if self.request != Some(view.request) {
            *self = Self {
                request: Some(view.request),
                name: view.name.clone(),
                ..Default::default()
            };
        }
        let mut commands = vec![];
        let mut emit = |command| {
            commands.push(AppCommand::Install {
                request: view.request,
                command,
            })
        };
        egui::Window::new("Install VectrCap destination")
            .collapsible(false)
            .resizable(true)
            .show(ctx, |ui| {
                ui.label(format!("Proposed name: {}",view.name));
                ui.label(format!("Server: {}",view.origin));
                ui.label("Uploader token: ********");
                match view.state {
                    InstallState::AwaitingVerifyConsent => {
                        if view.verification==Verification::Blocked {
                            ui.colored_label(ui.visuals().error_fg_color,"Verification failed. Installation is blocked.");
                        }
                        ui.checkbox(&mut self.consent,"Allow sending this token to the displayed server for verification");
                        if ui.add_enabled(self.consent,egui::Button::new("Verify / Continue")).clicked() {
                            emit(InstallCommand::Verify);
                        }
                    }
                    InstallState::Verifying => {
                        ui.spinner();
                        ui.label("Verifying with the displayed server...");
                    }
                    InstallState::AwaitingInstall => {
                        if view.verification==Verification::Verified {
                            ui.label(format!("Verified uploader: {}",view.uploader_name.as_deref().unwrap_or("")));
                        } else {
                            ui.label("Unverified: uploader identity has not been confirmed (unsupported endpoint, offline or TLS failure).");
                            if ui.button("Retry verification").clicked() {
                                emit(InstallCommand::Verify);
                            }
                            if ui.button("Explicitly allow saving as Unverified").clicked() {
                                emit(InstallCommand::AllowUnverified);
                            }
                        }
                        ui.label("Local destination name");
                        ui.text_edit_singleline(&mut self.name);
                        ui.checkbox(&mut self.set_default,"Set as default destination");
                        if ui.add_enabled(view.install_allowed,egui::Button::new("Confirm add destination")).clicked() {
                            emit(InstallCommand::Install(InstallChoice::Add{name:self.name.clone(),set_default:self.set_default}));
                        }
                        if let Some(settings)=settings {
                            for destination in settings.destinations.iter().filter(|d|d.name.trim().eq_ignore_ascii_case(view.name.trim()) && d.origin.as_str()==view.origin) {
                                if ui.button(format!("Replace token for {}...",destination.name)).clicked() {
                                    self.confirm_replace=Some(destination.id);
                                }
                                if self.confirm_replace==Some(destination.id) {
                                    ui.label("Replace this destination's saved token? Active uploads or workflows block replacement.");
                                    if ui.add_enabled(view.install_allowed,egui::Button::new("Confirm replace token")).clicked() {
                                        emit(InstallCommand::Install(InstallChoice::Replace(destination.id)));
                                    }
                                }
                            }
                        }
                    }
                    InstallState::Installed => {
                        ui.label(if view.verification==Verification::Unverified{"Destination installed as Unverified. No upload was requested."}else{"Destination installed. No upload was requested."});
                        if view.can_delete_source {
                            ui.checkbox(&mut self.confirm_delete,"Delete downloaded config file (optional)");
                            if self.confirm_delete {
                                ui.label("Delete only the unchanged original. This does not guarantee secure erasure.");
                                if ui.button("Confirm delete downloaded config").clicked() {
                                    emit(InstallCommand::DeleteSource);
                                }
                            }
                        }
                    }
                    _=>{}
                }
                if ui.button(if view.state==InstallState::Installed{"Close"}else{"Cancel"}).clicked() {
                    emit(InstallCommand::Close);
                }
            });
        commands
    }
}
