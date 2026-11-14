use rusqlite::{Connection, params};
use uuid::Uuid;
use vectrcap::persistence::history::{History, browser::*};

fn seeded() -> (tempfile::TempDir, History, Vec<Uuid>) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("history.db");
    let history = History::open(&db).unwrap();
    let mut conn = Connection::open(db).unwrap();
    let tx = conn.transaction().unwrap();
    let mut ids = vec![];
    for i in 0..107 {
        let id = Uuid::new_v4();
        let artifact = Uuid::new_v4();
        ids.push(id);
        let kind = if i % 2 == 0 { "screenshot" } else { "file" };
        let name = if i == 0 {
            "literal %_' OR 1=1 --.png".to_owned()
        } else {
            format!("capture-{i}.{}", if i % 2 == 0 { "png" } else { "txt" })
        };
        tx.execute(
            "INSERT INTO artifacts VALUES (?1,?2,?3,?4,0,zeroblob(32),100)",
            params![
                artifact.to_string(),
                "C:\\vectrcap-missing-test",
                name,
                kind
            ],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO history_items VALUES (?1,?2,0,100)",
            params![id.to_string(), artifact.to_string()],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    (dir, history, ids)
}
#[test]
fn pages_are_bounded_and_tie_stable_with_no_duplicates() {
    let (_dir, history, mut expected) = seeded();
    expected.sort_by_key(|a| std::cmp::Reverse(a.to_string()));
    let mut query = HistoryQuery::default();
    let mut seen = vec![];
    loop {
        let page = history.browse(&query).unwrap();
        assert!(page.records.len() <= 50);
        seen.extend(page.records.iter().map(|r| r.id));
        if !page.has_more {
            break;
        }
        let last = page.records.last().unwrap();
        query.after = Some(HistoryCursor::at(last));
    }
    assert_eq!(seen, expected);
}
#[test]
fn search_is_literal_filters_combine_and_favorites_persist_without_jobs() {
    let (dir, history, ids) = seeded();
    let query = HistoryQuery {
        search: "%_' OR 1=1 --".into(),
        ..Default::default()
    };
    let page = history.browse(&query).unwrap();
    assert_eq!(page.records.len(), 1);
    assert_eq!(page.records[0].id, ids[0]);
    assert!(page.records[0].job.is_none());
    history.set_favorite(ids[0], true).unwrap();
    history.set_favorite(ids[1], true).unwrap();
    let query = HistoryQuery {
        favorites_only: true,
        kind: HistoryType::Images,
        ..Default::default()
    };
    let page = history.browse(&query).unwrap();
    assert_eq!(page.records.len(), 1);
    assert_eq!(page.records[0].id, ids[0]);
    assert!(!page.records[0].local_available);
    drop(history);
    let history = History::open(&dir.path().join("history.db")).unwrap();
    assert_eq!(history.browse(&query).unwrap().records[0].id, ids[0]);
    history.set_favorite(ids[0], false).unwrap();
    assert!(history.browse(&query).unwrap().records.is_empty());
    let conn = Connection::open(dir.path().join("history.db")).unwrap();
    assert_eq!(
        conn.query_row("SELECT count(*) FROM artifacts", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        107
    );
}
#[test]
fn invalid_queries_and_missing_favorites_fail_closed() {
    let (_dir, history, _) = seeded();
    for search in ["x".repeat(257), "a\nb".into()] {
        assert!(
            history
                .browse(&HistoryQuery {
                    search,
                    ..Default::default()
                })
                .is_err()
        );
    }
    assert!(history.set_favorite(Uuid::new_v4(), true).is_err());
    assert!(history.set_favorite(Uuid::nil(), true).is_err());
}

#[test]
fn every_sort_pages_stably_and_date_status_filters_share_the_collection() {
    let (_dir, history, _) = seeded();
    for sort in [
        HistorySort::Newest,
        HistorySort::Oldest,
        HistorySort::Name,
        HistorySort::Size,
    ] {
        let mut q = HistoryQuery {
            sort,
            ..Default::default()
        };
        let mut records = vec![];
        loop {
            let page = history.browse(&q).unwrap();
            records.extend(page.records.iter().cloned());
            if !page.has_more {
                break;
            }
            q.after = Some(HistoryCursor::at(page.records.last().unwrap()));
        }
        assert_eq!(records.len(), 107);
        let unique: std::collections::HashSet<_> = records.iter().map(|r| r.id).collect();
        assert_eq!(unique.len(), 107);
        for pair in records.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            match sort {
                HistorySort::Newest => {
                    assert!((a.created_ms, a.id.to_string()) > (b.created_ms, b.id.to_string()))
                }
                HistorySort::Oldest => {
                    assert!((a.created_ms, a.id.to_string()) < (b.created_ms, b.id.to_string()))
                }
                HistorySort::Name => assert!(
                    (a.name.to_ascii_lowercase(), a.id.to_string())
                        < (b.name.to_ascii_lowercase(), b.id.to_string())
                ),
                HistorySort::Size => {
                    assert!((a.size, a.id.to_string()) > (b.size, b.id.to_string()))
                }
            }
        }
    }
    assert!(
        history
            .browse(&HistoryQuery {
                since_ms: Some(101),
                ..Default::default()
            })
            .unwrap()
            .records
            .is_empty()
    );
    assert!(
        history
            .browse(&HistoryQuery {
                until_ms: Some(99),
                ..Default::default()
            })
            .unwrap()
            .records
            .is_empty()
    );
    assert!(
        !history
            .browse(&HistoryQuery {
                since_ms: Some(100),
                until_ms: Some(100),
                status: HistoryStatus::LocalOnly,
                ..Default::default()
            })
            .unwrap()
            .records
            .is_empty()
    );
    assert!(
        history
            .browse(&HistoryQuery {
                status: HistoryStatus::Uploaded,
                ..Default::default()
            })
            .unwrap()
            .records
            .is_empty()
    );
    assert!(
        history
            .browse(&HistoryQuery {
                since_ms: Some(101),
                until_ms: Some(100),
                ..Default::default()
            })
            .is_err()
    );
}
#[test]
fn removing_local_only_history_does_not_delete_artifacts_or_files() {
    let (dir, history, ids) = seeded();
    history.set_favorite(ids[0], true).unwrap();
    history.remove_history(&[ids[0], ids[1]]).unwrap();
    assert!(history.history_record(ids[0]).unwrap().is_none());
    let conn = Connection::open(dir.path().join("history.db")).unwrap();
    assert_eq!(
        conn.query_row("SELECT count(*) FROM artifacts", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        107
    );
    assert!(history.remove_history(&[ids[2], Uuid::new_v4()]).is_err());
    assert!(history.history_record(ids[2]).unwrap().is_some());
}
#[test]
fn legacy_view_preferences_default_to_gallery_with_preview() {
    use vectrcap::persistence::settings::{HistoryLayout, Settings};
    let mut value = serde_json::to_value(Settings::default()).unwrap();
    value.as_object_mut().unwrap().remove("history");
    let settings: Settings = serde_json::from_value(value).unwrap();
    assert_eq!(settings.history.layout, HistoryLayout::Gallery);
    assert!(settings.history.preview);
}
#[test]
fn date_inputs_are_inclusive_and_reject_reversed_or_malformed_ranges() {
    use vectrcap::ui::history::date_range;
    let (a, b) = date_range("2026-11-14", "2026-11-14").unwrap();
    assert!(b.unwrap() > a.unwrap());
    assert!(date_range("2026-11-15", "2026-11-14").is_err());
    assert!(date_range("bad", "").is_err());
    assert_eq!(date_range("", "").unwrap(), (None, None));
}

#[test]
fn imported_image_classification_does_not_change_file_ownership() {
    let (dir, history, ids) = seeded();
    let conn = Connection::open(dir.path().join("history.db")).unwrap();
    conn.execute("UPDATE artifacts SET display_name='Imported.PNG' WHERE id=(SELECT artifact_id FROM history_items WHERE id=?1)",[ids[1].to_string()]).unwrap();
    let rows = history
        .browse(&HistoryQuery {
            kind: HistoryType::Images,
            search: "Imported".into(),
            ..Default::default()
        })
        .unwrap()
        .records;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].artifact.kind,
        vectrcap::app::types::ArtifactKind::File
    );
}
