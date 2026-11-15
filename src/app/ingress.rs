//! Durable path-only inbox. SQLite commit precedes ACK; completed IDs are tombstones.
use super::{cli::*, destinations::DestinationSnapshot, error::AppError};
use crate::platform::windows::ipc::{decode_frame, encode_frame};
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::path::Path;
use uuid::Uuid;
pub struct Ingress {
    db: Connection,
}
pub struct IngressAck {
    pub request_id: Uuid,
    pub duplicate: bool,
}
pub struct PendingIngress {
    pub request: LaunchRequest,
    pub destination: Option<DestinationSnapshot>,
    pub context: Option<crate::workflow::runner::WorkflowContext>,
}
impl Ingress {
    pub fn open(path: &Path) -> Result<Self, AppError> {
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(2))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS ingress(id TEXT PRIMARY KEY, frame BLOB NOT NULL, destination TEXT, context TEXT, kind INTEGER NOT NULL, done INTEGER NOT NULL DEFAULT 0); CREATE TABLE IF NOT EXISTS ingress_rejected(id TEXT NOT NULL, slot INTEGER NOT NULL, PRIMARY KEY(id,slot));")?;
        Ok(Self { db })
    }
    pub fn accept(&mut self, request: LaunchRequest) -> Result<IngressAck, AppError> {
        self.accept_context(request, None, None)
    }
    pub(crate) fn accept_context(
        &mut self,
        request: LaunchRequest,
        destination: Option<DestinationSnapshot>,
        context: Option<&crate::workflow::runner::WorkflowContext>,
    ) -> Result<IngressAck, AppError> {
        let frame = encode_frame(&request)?;
        let id = request.request_id;
        let import = matches!(request.kind, LaunchRequestKind::ImportDestination { .. });
        let tx = self.db.transaction()?;
        let existing: Option<(Vec<u8>, bool)> = tx
            .query_row(
                "SELECT frame,done FROM ingress WHERE id=?1",
                [id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((existing, done)) = existing {
            if existing
                != if done {
                    Sha256::digest(&frame).to_vec()
                } else {
                    frame
                }
            {
                return Err(AppError::InvalidInput(
                    "request identifier reused for different selection",
                ));
            }
            return Ok(IngressAck {
                request_id: id,
                duplicate: true,
            });
        }
        let pending: i64 = tx.query_row(
            "SELECT count(*) FROM ingress WHERE kind=?1 AND done=0",
            [i32::from(import)],
            |r| r.get(0),
        )?;
        if pending >= if import { 16 } else { 32 } {
            return Err(AppError::InUse("pending ingress capacity reached"));
        }
        let destination = if import {
            None
        } else {
            destination
                .map(|d| serde_json::to_string(&d))
                .transpose()
                .map_err(|_| AppError::Persistence("invalid destination snapshot"))?
        };
        let context = (if import { None } else { context })
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| AppError::Persistence("invalid ingress context"))?;
        tx.execute(
            "INSERT INTO ingress(id,frame,destination,context,kind) VALUES(?1,?2,?3,?4,?5)",
            params![
                id.to_string(),
                frame,
                destination,
                context,
                i32::from(import)
            ],
        )?;
        tx.commit()?;
        Ok(IngressAck {
            request_id: id,
            duplicate: false,
        })
    }
    pub fn pending(&self) -> Result<Vec<PendingIngress>, AppError> {
        let mut stmt = self.db.prepare(
            "SELECT frame,destination,context FROM ingress WHERE done=0 ORDER BY rowid LIMIT 48",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?;
        let mut pending = vec![];
        for row in rows {
            let (frame, destination, context) = row?;
            pending.push(PendingIngress {
                request: decode_frame(&frame)?,
                destination: destination
                    .map(|d| serde_json::from_str(&d))
                    .transpose()
                    .map_err(|_| AppError::Persistence("invalid ingress snapshot"))?,
                context: context
                    .map(|d| serde_json::from_str(&d))
                    .transpose()
                    .map_err(|_| AppError::Persistence("invalid ingress context"))?,
            });
        }
        Ok(pending)
    }
    pub(crate) fn save_context(
        &mut self,
        id: Uuid,
        context: &crate::workflow::runner::WorkflowContext,
    ) -> Result<(), AppError> {
        let data = serde_json::to_string(context)
            .map_err(|_| AppError::Persistence("invalid ingress context"))?;
        let destination = context
            .destination
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| AppError::Persistence("invalid ingress destination"))?;
        self.db.execute(
            "UPDATE ingress SET context=?1,destination=?2 WHERE id=?3 AND done=0",
            params![data, destination, id.to_string()],
        )?;
        Ok(())
    }
    pub(crate) fn reject_slot(&mut self, id: Uuid, index: usize) -> Result<(), AppError> {
        self.db.execute(
            "INSERT OR IGNORE INTO ingress_rejected(id,slot) VALUES(?1,?2)",
            params![id.to_string(), index as i64],
        )?;
        Ok(())
    }
    pub(crate) fn rejected(&self, id: Uuid, index: usize) -> Result<bool, AppError> {
        Ok(self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM ingress_rejected WHERE id=?1 AND slot=?2)",
            params![id.to_string(), index as i64],
            |r| r.get(0),
        )?)
    }
    pub fn complete(&mut self, id: Uuid) -> Result<(), AppError> {
        let tx = self.db.transaction()?;
        let frame: Option<Vec<u8>> = tx
            .query_row(
                "SELECT frame FROM ingress WHERE id=?1 AND done=0",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(frame) = frame {
            let digest = Sha256::digest(frame).to_vec();
            tx.execute(
                "UPDATE ingress SET done=1,frame=?1,destination=NULL,context=NULL WHERE id=?2",
                params![digest, id.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}
use rusqlite::OptionalExtension;
