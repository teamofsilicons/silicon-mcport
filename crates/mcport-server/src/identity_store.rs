//! Plaintext identity tables: cached accounts, IAM-era identity links, webhook
//! deliveries already handled and Silicons' allow lists. None of these hold
//! secrets; encrypted application records stay in `records`.
use crate::{
    accounts::AccountRow,
    error::{Error, Result},
    state::now,
    store::Store,
};
use rusqlite::{OptionalExtension, Row, params};

const ACCOUNT_COLUMNS: &str = "uuid,kind,id,display_name,pfp_url,status,custodian_uuid,custodian_id,version,revoked_before,synced_at_ms,looked_up_at,last_fid,updated_at";

fn account_row(row: &Row<'_>) -> rusqlite::Result<AccountRow> {
    Ok(AccountRow {
        uuid: row.get(0)?,
        kind: row.get(1)?,
        id: row.get(2)?,
        display_name: row.get(3)?,
        pfp_url: row.get(4)?,
        status: row.get(5)?,
        custodian_uuid: row.get(6)?,
        custodian_id: row.get(7)?,
        version: row.get(8)?,
        revoked_before: row.get(9)?,
        synced_at_ms: row.get(10)?,
        looked_up_at: row.get(11)?,
        last_fid: row.get(12)?,
        updated_at: row.get(13)?,
    })
}
pub(crate) fn write_account(db: &rusqlite::Connection, a: &AccountRow) -> Result<()> {
    db.execute(
        &format!(
            "INSERT OR REPLACE INTO accounts({ACCOUNT_COLUMNS}) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)"
        ),
        params![
            a.uuid,
            a.kind,
            a.id,
            a.display_name,
            a.pfp_url,
            a.status,
            a.custodian_uuid,
            a.custodian_id,
            a.version,
            a.revoked_before,
            a.synced_at_ms,
            a.looked_up_at,
            a.last_fid,
            a.updated_at
        ],
    )?;
    Ok(())
}
pub(crate) fn read_account(db: &rusqlite::Connection, uuid: &str) -> Result<Option<AccountRow>> {
    Ok(db
        .query_row(
            &format!("SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE uuid=?"),
            [uuid],
            account_row,
        )
        .optional()?)
}

impl Store {
    pub fn account(&self, uuid: &str) -> Result<Option<AccountRow>> {
        let db = self.db.lock().map_err(|_| Error::internal())?;
        read_account(&db, uuid)
    }
    /// Atomically read, change and write one account row. `change` receives the
    /// current row (`None` if MCPort never saw the account) and returns the row to
    /// store, or `None` to leave it unchanged. Returns the stored row.
    pub fn update_account(
        &self,
        uuid: &str,
        change: impl FnOnce(Option<AccountRow>) -> Option<AccountRow>,
    ) -> Result<Option<AccountRow>> {
        let mut db = self.db.lock().map_err(|_| Error::internal())?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current = read_account(&tx, uuid)?;
        let result = match change(current.clone()) {
            Some(mut next) => {
                next.uuid = uuid.into();
                if current.as_ref() != Some(&next) {
                    next.updated_at = now();
                    write_account(&tx, &next)?;
                }
                Some(next)
            }
            None => current,
        };
        tx.commit()?;
        Ok(result)
    }
    /// The Silicons MCPort knows whose custodian is `custodian_uuid`.
    pub fn silicons_of(&self, custodian_uuid: &str) -> Result<Vec<AccountRow>> {
        let db = self.db.lock().map_err(|_| Error::internal())?;
        let mut statement = db.prepare(&format!(
            "SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE custodian_uuid=? AND kind='silicon' ORDER BY id"
        ))?;
        Ok(statement
            .query_map([custodian_uuid], account_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// IAM-era principal ids linked to `uuid` (for host registries not yet migrated).
    pub fn legacy_ids(&self, uuid: &str) -> Result<Vec<String>> {
        let db = self.db.lock().map_err(|_| Error::internal())?;
        let mut statement = db.prepare(
            "SELECT iam_principal_id FROM identity_links WHERE accounts_uuid=? ORDER BY iam_principal_id",
        )?;
        Ok(statement
            .query_map([uuid], |row| row.get(0))?
            .collect::<std::result::Result<Vec<String>, _>>()?)
    }
    /// The account an IAM-era principal id was linked to at cutover, if any.
    pub fn linked_uuid(&self, iam_principal_id: &str) -> Result<Option<String>> {
        let db = self.db.lock().map_err(|_| Error::internal())?;
        Ok(db
            .query_row(
                "SELECT accounts_uuid FROM identity_links WHERE iam_principal_id=?",
                [iam_principal_id],
                |row| row.get(0),
            )
            .optional()?)
    }
    /// Whether this webhook event id was already handled.
    pub fn webhook_handled(&self, event_id: &str) -> Result<bool> {
        let db = self.db.lock().map_err(|_| Error::internal())?;
        Ok(db
            .query_row(
                "SELECT 1 FROM accounts_webhook_events WHERE event_id=?",
                [event_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }
    /// Remember a handled event. Returns false if it was already recorded.
    pub fn record_webhook(
        &self,
        event_id: &str,
        event_type: &str,
        occurred_at_ms: Option<i64>,
    ) -> Result<bool> {
        let db = self.db.lock().map_err(|_| Error::internal())?;
        Ok(db.execute(
            "INSERT OR IGNORE INTO accounts_webhook_events(event_id,event_type,occurred_at_ms,received_at) VALUES(?,?,?,?)",
            params![event_id, event_type, occurred_at_ms, now()],
        )? == 1)
    }

    /// Accounts a Silicon accepts shares from although they are outside its circle.
    pub fn allowances(&self, silicon_uuid: &str) -> Result<Vec<(String, String, i64)>> {
        let db = self.db.lock().map_err(|_| Error::internal())?;
        let mut statement = db.prepare(
            "SELECT account_uuid,created_by,created_at FROM silicon_allowances WHERE silicon_uuid=? ORDER BY created_at,account_uuid",
        )?;
        Ok(statement
            .query_map([silicon_uuid], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }
    pub fn allows(&self, silicon_uuid: &str, account_uuid: &str) -> Result<bool> {
        let db = self.db.lock().map_err(|_| Error::internal())?;
        Ok(db
            .query_row(
                "SELECT 1 FROM silicon_allowances WHERE silicon_uuid=? AND account_uuid=?",
                params![silicon_uuid, account_uuid],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }
    pub fn allow(&self, silicon_uuid: &str, account_uuid: &str, created_by: &str) -> Result<i64> {
        let db = self.db.lock().map_err(|_| Error::internal())?;
        db.execute(
            "INSERT OR IGNORE INTO silicon_allowances(silicon_uuid,account_uuid,created_by,created_at) VALUES(?,?,?,?)",
            params![silicon_uuid, account_uuid, created_by, now()],
        )?;
        Ok(db.query_row(
            "SELECT created_at FROM silicon_allowances WHERE silicon_uuid=? AND account_uuid=?",
            params![silicon_uuid, account_uuid],
            |row| row.get(0),
        )?)
    }
    pub fn disallow(&self, silicon_uuid: &str, account_uuid: &str) -> Result<bool> {
        let db = self.db.lock().map_err(|_| Error::internal())?;
        Ok(db.execute(
            "DELETE FROM silicon_allowances WHERE silicon_uuid=? AND account_uuid=?",
            params![silicon_uuid, account_uuid],
        )? == 1)
    }
    /// Remove every allowance a deleted account appears in.
    pub fn forget_allowances(&self, uuid: &str) -> Result<usize> {
        let db = self.db.lock().map_err(|_| Error::internal())?;
        Ok(db.execute(
            "DELETE FROM silicon_allowances WHERE silicon_uuid=?1 OR account_uuid=?1",
            [uuid],
        )?)
    }
    /// Remove the identity links of a deleted account.
    pub fn forget_identity_links(&self, uuid: &str) -> Result<usize> {
        let db = self.db.lock().map_err(|_| Error::internal())?;
        Ok(db.execute("DELETE FROM identity_links WHERE accounts_uuid=?", [uuid])?)
    }
}
