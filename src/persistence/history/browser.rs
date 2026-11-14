//! Bounded history browsing; favorites never alter artifact retention.
use super::*;

pub const HISTORY_PAGE_SIZE: usize = 50;
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum HistoryType {
    #[default]
    All,
    Images,
    Recordings,
    Files,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HistoryCursor {
    pub created_ms: i64,
    pub id: Uuid,
}
#[derive(Clone, Default, PartialEq, Eq)]
pub struct HistoryQuery {
    pub search: String,
    pub kind: HistoryType,
    pub favorites_only: bool,
    pub after: Option<HistoryCursor>,
}
#[derive(Clone)]
pub struct HistoryRecord {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub size: u64,
    pub created_ms: i64,
    pub favorite: bool,
    pub local_available: bool,
    pub job: Option<UploadJob>,
}
#[derive(Clone, Default)]
pub struct HistoryPage {
    pub records: Vec<HistoryRecord>,
    pub has_more: bool,
}
impl History {
    pub fn browse(&self, query: &HistoryQuery) -> Result<HistoryPage, AppError> {
        if query.search.len() > 256
            || query.search.chars().any(char::is_control)
            || query.after.is_some_and(|c| c.id.is_nil())
        {
            return Err(AppError::InvalidInput("invalid history query"));
        }
        let kind = match query.kind {
            HistoryType::All => "all",
            HistoryType::Images => "images",
            HistoryType::Recordings => "recordings",
            HistoryType::Files => "files",
        };
        let cursor = query.after.map(|c| c.id.to_string());
        let conn = self.lock()?;
        let mut stmt = conn.prepare("SELECT h.id, a.display_name, a.kind, a.size, h.created_ms, h.favorite,
            (SELECT j.id FROM jobs j WHERE j.artifact_id=a.id ORDER BY j.acceptance_ordinal DESC LIMIT 1), a.path
            FROM history_items h JOIN artifacts a ON a.id=h.artifact_id
            WHERE (?1='' OR instr(lower(a.display_name),lower(?1))>0 OR instr(a.kind,lower(?1))>0
              OR EXISTS(SELECT 1 FROM jobs s WHERE s.artifact_id=a.id AND s.status='completed' AND instr(lower(s.receipt_url),lower(?1))>0))
            AND (?2='all' OR (?2='images' AND a.kind='screenshot') OR (?2='recordings' AND a.kind IN ('gif','mp4')) OR (?2='files' AND a.kind='file'))
            AND (?3=0 OR h.favorite=1)
            AND (?4 IS NULL OR h.created_ms<?5 OR (h.created_ms=?5 AND h.id<?4))
            ORDER BY h.created_ms DESC,h.id DESC LIMIT 51")?;
        let raw = stmt
            .query_map(
                params![
                    query.search,
                    kind,
                    query.favorites_only,
                    cursor,
                    query.after.map(|c| c.created_ms)
                ],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, bool>(5)?,
                        r.get::<_, Option<String>>(6)?,
                        r.get::<_, String>(7)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        drop(conn);
        let has_more = raw.len() > HISTORY_PAGE_SIZE;
        let mut records = Vec::new();
        for (id, name, kind, size, created_ms, favorite, job, path) in
            raw.into_iter().take(HISTORY_PAGE_SIZE)
        {
            let job = job
                .map(|id| self.job(JobId(parse_uuid(&id)?)))
                .transpose()?
                .flatten();
            records.push(HistoryRecord {
                id: parse_uuid(&id)?,
                name,
                kind,
                size: u64::try_from(size).map_err(|_| corrupt())?,
                created_ms,
                favorite,
                local_available: std::path::Path::new(&path).is_file(),
                job,
            });
        }
        Ok(HistoryPage { records, has_more })
    }
    pub fn set_favorite(&self, id: Uuid, favorite: bool) -> Result<(), AppError> {
        if id.is_nil() {
            return Err(AppError::InvalidInput("invalid history item"));
        }
        if self.lock()?.execute(
            "UPDATE history_items SET favorite=?1 WHERE id=?2",
            params![favorite, id.to_string()],
        )? != 1
        {
            return Err(AppError::InvalidInput("history item no longer available"));
        }
        Ok(())
    }
}
