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
            format!("capture-{i}.png")
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
        query.after = Some(HistoryCursor {
            created_ms: last.created_ms,
            id: last.id,
        });
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
