use std::fs;
use std::io::Write;

use chrono::{DateTime, Local, TimeZone};
use tempfile::{NamedTempFile, TempDir};
use vectrcap::app::destinations::{DestinationSelection, Destinations, resolve_destination};
use vectrcap::app::error::AppError;
use vectrcap::app::types::ArtifactKind;
use vectrcap::persistence::artifacts::ArtifactStore;
use vectrcap::persistence::filename::{FilenamePreset, OutputFormat};
use vectrcap::persistence::history::History;
use vectrcap::persistence::settings::ServerOrigin;

fn at(h: u32, mi: u32, s: u32) -> DateTime<Local> {
    Local
        .with_ymd_and_hms(2026, 11, 14, h, mi, s)
        .single()
        .unwrap()
}

fn preset(prefix: &str, template: &str) -> FilenamePreset {
    FilenamePreset {
        prefix: prefix.into(),
        template: template.into(),
    }
}

fn temp_with(dir: &std::path::Path, bytes: &[u8]) -> NamedTempFile {
    let mut tmp = NamedTempFile::new_in(dir).unwrap();
    tmp.write_all(bytes).unwrap();
    tmp
}

#[test]
fn default_name_contains_time_id_and_real_extension() {
    let p = FilenamePreset::default();
    assert_eq!(p.prefix, "VectrCap");
    assert_eq!(p.template, "{prefix}_{date}_{time}_{id}");
    p.validate().unwrap();
    assert_eq!(
        p.render(at(8, 3, 9), "1a2b3c4d", OutputFormat::Png)
            .unwrap(),
        "VectrCap_2026-11-14_08-03-09_1a2b3c4d.png"
    );
    assert!(
        p.render(at(8, 3, 9), "1a2b3c4d", OutputFormat::Mp4)
            .unwrap()
            .ends_with("_1a2b3c4d.mp4")
    );
    assert!(
        p.render(at(8, 3, 9), "1a2b3c4d", OutputFormat::Gif)
            .unwrap()
            .ends_with(".gif")
    );
    assert!(
        p.render(at(8, 3, 9), "1a2b3c4d", OutputFormat::Jpeg)
            .unwrap()
            .ends_with(".jpg")
    );
    assert!(
        p.render(at(8, 3, 9), "1a2b3c4d", OutputFormat::WebP)
            .unwrap()
            .ends_with(".webp")
    );
}

#[test]
fn unknown_tokens_and_unsafe_windows_names_rejected() {
    let bad_templates = [
        "",
        "{user}_{id}",
        "{ID}",
        "{date_{id}",
        "date}_{id}",
        "{prefix}/{id}",
        "..\\{id}",
        "{id}:stream",
        "CON.{id}",
        "nul.{id}",
        "Com1.{id}",
        "lpt9 .{id}",
        "{id}.",
        "{id} ",
        "a*{id}",
        "a?{id}",
        "<{id}>",
        "a|{id}",
        "a\"{id}",
        "\u{1}{id}",
        "${env:USERNAME}{id}",
    ];
    for t in bad_templates {
        assert!(
            matches!(
                preset("VectrCap", t).validate(),
                Err(AppError::InvalidInput(_))
            ),
            "template {t:?} must be rejected"
        );
    }
    for p in ["a/b", "..\\x", "x:y", "\t", "bad>"] {
        assert!(
            preset(p, "{prefix}_{id}").validate().is_err(),
            "prefix {p:?} must be rejected"
        );
    }
    assert!(preset(&"p".repeat(49), "{prefix}_{id}").validate().is_err());
    preset(&"p".repeat(48), "{prefix}_{id}").validate().unwrap();
    assert!(
        preset("", &format!("{}{{id}}", "t".repeat(253)))
            .validate()
            .is_err()
    );
    // Empty prefix is fine when the result is still a valid name.
    preset("", "{prefix}_{date}_{time}_{id}")
        .validate()
        .unwrap();
    // An invalid preset never renders a name.
    assert!(
        preset("VectrCap", "CON.{id}")
            .render(at(1, 2, 3), "abcdef01", OutputFormat::Png)
            .is_err()
    );
}

#[test]
fn omitted_id_still_gets_unique_suffix() {
    let p = preset("VectrCap", "{prefix}_{date}");
    p.validate().unwrap();
    let one = p
        .render(at(8, 3, 9), "deadbeef", OutputFormat::Png)
        .unwrap();
    let two = p
        .render(at(8, 3, 9), "0badf00d", OutputFormat::Png)
        .unwrap();
    assert_eq!(one, "VectrCap_2026-11-14_deadbeef.png");
    assert_ne!(one, two);
}

#[test]
fn collision_never_overwrites() {
    let dir = TempDir::new().unwrap();
    let p = FilenamePreset::default();
    let taken = dir.path().join("VectrCap_2026-11-14_08-03-09_aaaaaaaa.png");
    fs::write(&taken, b"previous capture").unwrap();

    let mut suffixes = ["aaaaaaaa", "bbbbbbbb"].into_iter().map(String::from);
    let (name, path) = p
        .persist_unique(
            temp_with(dir.path(), b"new"),
            dir.path(),
            at(8, 3, 9),
            OutputFormat::Png,
            &mut || suffixes.next().unwrap(),
        )
        .unwrap();
    assert_eq!(name, "VectrCap_2026-11-14_08-03-09_bbbbbbbb.png");
    assert_eq!(path, dir.path().join(&name));
    assert_eq!(fs::read(&taken).unwrap(), b"previous capture");
    assert_eq!(fs::read(&path).unwrap(), b"new");

    // A generator that keeps colliding gives up after bounded retries.
    let result = p.persist_unique(
        temp_with(dir.path(), b"x"),
        dir.path(),
        at(8, 3, 9),
        OutputFormat::Png,
        &mut || "aaaaaaaa".to_string(),
    );
    assert!(matches!(
        result,
        Err(AppError::Io {
            kind: std::io::ErrorKind::AlreadyExists,
            ..
        })
    ));
    assert_eq!(fs::read(&taken).unwrap(), b"previous capture");
    let leftovers = fs::read_dir(dir.path()).unwrap().count();
    assert_eq!(
        leftovers, 2,
        "failed attempts must not leave temporary files behind"
    );
}

#[test]
fn utf16_name_bound_is_checked() {
    // 48 scalars (valid prefix) but 96 UTF-16 code units.
    let wide = "😀".repeat(48);
    preset(&wide, "{prefix}_{id}").validate().unwrap();
    assert!(matches!(
        preset(&wide, "{prefix}{prefix}_{id}").validate(),
        Err(AppError::InvalidInput(_))
    ));
}

#[test]
fn retry_reuses_persisted_name() {
    let dir = TempDir::new().unwrap();
    let p = FilenamePreset::default();
    let name = p
        .render(at(8, 3, 9), "1a2b3c4d", OutputFormat::Png)
        .unwrap();
    let work = dir.path().join(&name);
    fs::write(&work, b"png bytes").unwrap();

    let store = ArtifactStore::new(dir.path().join("spool")).unwrap();
    let artifact = store.stage_file(&work, ArtifactKind::Screenshot).unwrap();
    assert_eq!(artifact.display_name, name);

    let mut d = Destinations::default();
    let id = d
        .add(
            "Phone",
            ServerOrigin::parse("https://a.example").unwrap(),
            None,
        )
        .unwrap();
    d.bind_credential(id, &Default::default()).unwrap();
    let dest = resolve_destination(
        DestinationSelection::Inherit,
        DestinationSelection::Inherit,
        &d,
    )
    .unwrap();
    let db = dir.path().join("history.sqlite3");
    let job_id = History::open(&db)
        .unwrap()
        .create_job(&artifact, &dest)
        .unwrap()
        .id;

    // A later preset/time would produce a different name; the job keeps its own.
    let later = preset("Other", "{prefix}_{id}")
        .render(at(9, 0, 0), "99999999", OutputFormat::Png)
        .unwrap();
    assert_ne!(later, name);
    let recovered = History::open(&db).unwrap().job(job_id).unwrap().unwrap();
    assert_eq!(recovered.artifact.display_name, name);
    assert_eq!(recovered.artifact, artifact);
}
