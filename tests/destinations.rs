use std::collections::HashSet;
use std::fs;

use tempfile::TempDir;
use vectrcap::app::destinations::{
    DestinationId, DestinationSelection, DestinationSnapshot, Destinations, resolve_destination,
};
use vectrcap::app::error::AppError;
use vectrcap::app::types::{Artifact, ArtifactKind};
use vectrcap::persistence::artifacts::ArtifactStore;
use vectrcap::persistence::history::{History, JobStatus};
use vectrcap::persistence::settings::{LinkOrigin, ServerOrigin};

use DestinationSelection::{Inherit, Specific};

fn origin(s: &str) -> ServerOrigin {
    ServerOrigin::parse(s).unwrap()
}

fn none() -> HashSet<DestinationId> {
    HashSet::new()
}

/// Adds a destination and binds a credential reference so it can resolve.
fn add_ready(d: &mut Destinations, name: &str, o: &str) -> DestinationId {
    let id = d.add(name, origin(o), None).unwrap();
    d.bind_credential(id, &none()).unwrap();
    id
}

struct Fixture {
    dir: TempDir,
    store: ArtifactStore,
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let store = ArtifactStore::new(dir.path().join("spool")).unwrap();
        Fixture { dir, store }
    }

    fn history(&self) -> History {
        History::open(&self.dir.path().join("history.sqlite3")).unwrap()
    }

    fn artifact(&self) -> Artifact {
        let src = self.dir.path().join("report.zip");
        fs::write(&src, b"zip bytes").unwrap();
        self.store.stage_file(&src, ArtifactKind::File).unwrap()
    }
}

#[test]
fn same_origin_destinations_have_distinct_ids_and_credentials() {
    let mut d = Destinations::default();
    let a = add_ready(&mut d, "Phone", "https://vectr.example");
    let b = add_ready(&mut d, "Laptop", "https://vectr.example");
    assert_ne!(a, b);

    let ka = d.get(a).unwrap().credential_ref.clone().unwrap();
    let kb = d.get(b).unwrap().credential_ref.clone().unwrap();
    assert_ne!(
        ka, kb,
        "same-origin uploaders must never share a credential key"
    );
    assert_eq!(ka.origin(), kb.origin());
    assert_eq!(ka.destination(), a);
    assert_eq!(kb.destination(), b);

    let sa = resolve_destination(Specific(a), Inherit, &d).unwrap();
    let sb = resolve_destination(Specific(b), Inherit, &d).unwrap();
    assert_eq!(sa.credential, ka);
    assert_eq!(sb.credential, kb);
}

#[test]
fn exactly_one_default_when_configured() {
    let mut d = Destinations::default();
    assert_eq!(d.default_id(), None);
    d.validate().unwrap();

    let a = d.add("Phone", origin("https://a.example"), None).unwrap();
    assert_eq!(d.default_id(), Some(a), "first destination becomes default");
    let b = d.add("Laptop", origin("https://b.example"), None).unwrap();
    assert_eq!(
        d.default_id(),
        Some(a),
        "creation alone never changes the default"
    );

    d.set_default(b).unwrap();
    assert_eq!(d.default_id(), Some(b));
    assert!(d.set_default(DestinationId::new_random()).is_err());
    assert_eq!(d.default_id(), Some(b));

    // Persisted data cannot smuggle in zero or dangling defaults.
    let mut v = serde_json::to_value(&d).unwrap();
    v["default"] = serde_json::Value::Null;
    assert!(serde_json::from_value::<Destinations>(v.clone()).is_err());
    v["default"] = serde_json::to_value(DestinationId::new_random()).unwrap();
    assert!(serde_json::from_value::<Destinations>(v).is_err());

    let empty_with_default = serde_json::json!({"items": [], "default": a});
    assert!(serde_json::from_value::<Destinations>(empty_with_default).is_err());

    let roundtrip: Destinations =
        serde_json::from_value(serde_json::to_value(&d).unwrap()).unwrap();
    assert_eq!(roundtrip, d);
}

#[test]
fn duplicate_or_control_character_names_rejected() {
    let mut d = Destinations::default();
    let a = d.add("Phone", origin("https://a.example"), None).unwrap();
    for bad in [
        "  phone ",
        "PHONE",
        "",
        "   ",
        "Bad\u{7}name",
        "Line\nbreak",
        "Tab\tname",
    ] {
        assert!(
            matches!(
                d.add(bad, origin("https://a.example"), None),
                Err(AppError::InvalidInput(_))
            ),
            "{bad:?} must be rejected"
        );
    }
    assert!(
        d.add(&"x".repeat(101), origin("https://a.example"), None)
            .is_err()
    );
    // The bound counts Unicode scalars, not bytes.
    let b = d
        .add(&"é".repeat(100), origin("https://a.example"), None)
        .unwrap();
    assert!(
        d.rename(b, " phone").is_err(),
        "rename must keep names unique"
    );
    d.rename(a, "PHONE").unwrap(); // a destination may change its own case
    assert_eq!(d.get(a).unwrap().name, "PHONE");

    for i in d.len()..32 {
        d.add(&format!("Dest {i}"), origin("https://a.example"), None)
            .unwrap();
    }
    assert_eq!(d.len(), 32);
    assert!(matches!(
        d.add("One too many", origin("https://a.example"), None),
        Err(AppError::InvalidInput(_))
    ));
}

#[test]
fn task_then_hotkey_then_default_precedence() {
    let mut d = Destinations::default();
    let c = add_ready(&mut d, "Default", "https://c.example");
    let a = add_ready(&mut d, "Task", "https://a.example");
    let b = add_ready(&mut d, "Hotkey", "https://b.example");
    assert_eq!(d.default_id(), Some(c));

    assert_eq!(
        resolve_destination(Specific(a), Specific(b), &d)
            .unwrap()
            .id,
        a
    );
    assert_eq!(resolve_destination(Inherit, Specific(b), &d).unwrap().id, b);
    assert_eq!(resolve_destination(Inherit, Inherit, &d).unwrap().id, c);
}

#[test]
fn missing_reference_never_uses_another_destination() {
    let mut d = Destinations::default();
    let needs_setup =
        |r: Result<DestinationSnapshot, AppError>| matches!(r, Err(AppError::NeedsSetup(_)));
    assert!(
        needs_setup(resolve_destination(Inherit, Inherit, &d)),
        "no destination configured"
    );

    let c = add_ready(&mut d, "Default", "https://c.example");
    let b = add_ready(&mut d, "Hotkey", "https://b.example");
    let gone = DestinationId::new_random();
    assert!(needs_setup(resolve_destination(
        Specific(gone),
        Specific(b),
        &d
    )));
    assert!(needs_setup(resolve_destination(
        Inherit,
        Specific(gone),
        &d
    )));

    // A tokenless (e.g. imported) destination is valid data but needs setup.
    let tokenless = d
        .add("Imported", origin("https://c.example"), None)
        .unwrap();
    d.validate().unwrap();
    assert!(needs_setup(resolve_destination(
        Specific(tokenless),
        Inherit,
        &d
    )));

    d.set_default(tokenless).unwrap();
    assert!(needs_setup(resolve_destination(Inherit, Inherit, &d)));
    assert_eq!(resolve_destination(Specific(c), Inherit, &d).unwrap().id, c);
}

#[test]
fn switch_default_does_not_retarget_accepted_work() {
    let fx = Fixture::new();
    let history = fx.history();
    let mut d = Destinations::default();
    let a = add_ready(&mut d, "Phone", "https://a.example");
    let b = add_ready(&mut d, "Laptop", "https://b.example");

    let accepted = resolve_destination(Inherit, Inherit, &d).unwrap();
    assert_eq!(accepted.id, a);
    let job = history.create_job(&fx.artifact(), &accepted).unwrap();

    d.set_default(b).unwrap();
    let reloaded = history.job(job.id).unwrap().unwrap();
    assert_eq!(reloaded.destination, accepted);
    assert_eq!(reloaded.destination.id, a);
}

#[test]
fn job_snapshot_survives_rename_domain_edit_and_restart() {
    let fx = Fixture::new();
    let mut d = Destinations::default();
    let a = d
        .add(
            "Phone",
            origin("https://vectr.example"),
            Some(LinkOrigin::parse("https://i.example").unwrap()),
        )
        .unwrap();
    d.bind_credential(a, &none()).unwrap();
    let snapshot = resolve_destination(Inherit, Inherit, &d).unwrap();
    let job_id = {
        let history = fx.history();
        history.create_job(&fx.artifact(), &snapshot).unwrap().id
    };

    d.rename(a, "Renamed").unwrap();
    d.set_link_domain(a, Some(LinkOrigin::parse("https://share.example").unwrap()))
        .unwrap();

    let reopened = fx.history();
    let jobs = reopened.recover_jobs().unwrap();
    let job = jobs.iter().find(|j| j.id == job_id).unwrap();
    assert_eq!(job.destination, snapshot);
    assert_eq!(job.destination.label, "Phone");
    assert_eq!(
        job.destination.link_domain.as_ref().unwrap().as_str(),
        "https://i.example"
    );
}

#[test]
fn active_references_block_identity_change_or_removal() {
    let fx = Fixture::new();
    let history = fx.history();
    let mut d = Destinations::default();
    let a = add_ready(&mut d, "Phone", "https://a.example");
    let b = add_ready(&mut d, "Laptop", "https://b.example");
    let job = history
        .create_job(
            &fx.artifact(),
            &resolve_destination(Specific(a), Inherit, &d).unwrap(),
        )
        .unwrap();

    let active = history.active_destination_ids().unwrap();
    assert!(active.contains(&a) && !active.contains(&b));
    let in_use = |r: Result<_, AppError>| matches!(r, Err(AppError::InUse(_)));
    assert!(in_use(d.change_origin(
        a,
        origin("https://moved.example"),
        &active
    )));
    assert!(in_use(d.bind_credential(a, &active).map(|_| ())));
    assert!(in_use(d.remove(a, Some(b), &active).map(|_| ())));
    assert_eq!(d.get(a).unwrap().origin, origin("https://a.example"));

    // Label/domain edits are future-only and remain allowed.
    d.rename(a, "Phone 2").unwrap();
    d.set_link_domain(a, None).unwrap();

    history.transition(job.id, JobStatus::Cancelled).unwrap();
    let active = history.active_destination_ids().unwrap();
    assert!(active.is_empty());

    // An origin change never carries the old token binding along.
    d.change_origin(a, origin("https://moved.example"), &active)
        .unwrap();
    assert!(d.get(a).unwrap().credential_ref.is_none());
    assert!(matches!(
        resolve_destination(Specific(a), Inherit, &d),
        Err(AppError::NeedsSetup(_))
    ));

    // Removing the default needs an explicit, valid replacement.
    assert!(d.remove(a, None, &active).is_err());
    assert!(d.remove(a, Some(a), &active).is_err());
    assert!(
        d.remove(a, Some(DestinationId::new_random()), &active)
            .is_err()
    );
    let removed = d.remove(a, Some(b), &active).unwrap();
    assert_eq!(removed.id, a);
    assert_eq!(d.default_id(), Some(b));
    d.remove(b, None, &active).unwrap();
    assert_eq!(d.default_id(), None);
    d.validate().unwrap();
}
