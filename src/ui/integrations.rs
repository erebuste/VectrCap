//! Explicit per-user registration controls; opening Settings never writes keys.
use crate::{
    app::error::AppError,
    platform::windows::{destination_association::DestinationAssociation, explorer::*},
};
#[derive(Default)]
pub struct IntegrationsPane {
    status: Option<Result<(RegistrationStatus, RegistrationStatus), AppError>>,
    error: Option<AppError>,
}
impl IntegrationsPane {
    pub fn draw(&mut self, ui: &mut eframe::egui::Ui) {
        ui.collapsing("Integrations", |ui| {
            ui.label("Explorer: Upload with Vectr (Windows 11 may show this under Show more options). Destination files: .vectrcap. Enable after installing VectrCap at a permanent location.");
            let executable=std::env::current_exe().map_err(|e|AppError::Io{stage:"locating executable",kind:e.kind()});
            let Ok(executable)=executable else {
                ui.label("Executable unavailable");
                return;
            };
            let registry=NativeRegistry;
            let explorer=ExplorerIntegration::new(&registry,executable.clone());
            let association=DestinationAssociation::new(&registry,executable);
            if ui.button("Refresh integration status").clicked() {
                self.status=Some(explorer.status().and_then(|a|association.status().map(|b|(a,b))));
            }
            if let Some(status)=&self.status {
                match status {
                    Ok((a,b))=>{ui.label(format!("Explorer: {a:?}; .vectrcap registration: {b:?}"));}
                    Err(error)=>{ui.label(error.to_string());}
                }
            }
            ui.label("Existing Windows defaults and UserChoice are preserved. A foreign default may require choosing VectrCap in Windows Open with.");
            for (label,action) in [
                ("Enable / repair Explorer upload",0),
                ("Disable Explorer upload",1),
                ("Enable / repair .vectrcap association",2),
                ("Disable .vectrcap association",3),
            ] {
                if ui.button(label).clicked() {
                    let result=match action {
                        0=>explorer.enable(),
                        1=>explorer.disable(),
                        2=>association.enable(),
                        _=>association.disable(),
                    };
                    match result {
                        Ok(())=>{
                            self.error=None;
                            self.status=Some(explorer.status().and_then(|a|association.status().map(|b|(a,b))));
                        }
                        Err(error)=>self.error=Some(error),
                    }
                }
            }
            if let Some(error)=&self.error {
                ui.colored_label(ui.visuals().error_fg_color,error.to_string());
            }
        });
    }
}
