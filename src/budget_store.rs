//! Opt-in, local SQLite budget accounting. No wallet, proof or payment authority.
//! Callers supply validated UTC days and stable, persisted operation IDs.
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use std::{
    fs::{File, OpenOptions},
    path::Path,
    time::Duration,
};

const SCHEMA: &str = "CREATE TABLE metadata (singleton INTEGER PRIMARY KEY CHECK(singleton=1), high_day INTEGER NOT NULL CHECK(high_day>=0));
            INSERT INTO metadata VALUES(1,0);
            CREATE TABLE reservations (
                id TEXT PRIMARY KEY NOT NULL, unit TEXT NOT NULL, day INTEGER NOT NULL CHECK(day>=0),
                face INTEGER NOT NULL CHECK(face>0), fee INTEGER NOT NULL CHECK(fee>=0), cap INTEGER NOT NULL CHECK(cap>=0),
                total INTEGER NOT NULL CHECK(total>0 AND total>=face AND total-face=fee));
            CREATE TABLE operations (
                id TEXT PRIMARY KEY NOT NULL, reservation TEXT NOT NULL REFERENCES reservations(id),
                kind TEXT NOT NULL CHECK(kind IN ('reserve','settle','release')),
                day INTEGER NOT NULL CHECK(day>=0), amount INTEGER NOT NULL CHECK(amount>=0));
            CREATE UNIQUE INDEX one_reserve ON operations(reservation) WHERE kind='reserve';
            CREATE UNIQUE INDEX one_terminal ON operations(reservation) WHERE kind!='reserve';
            CREATE TABLE sends (
                operation TEXT PRIMARY KEY NOT NULL REFERENCES reservations(id),
                network TEXT NOT NULL, vault TEXT NOT NULL, mint TEXT NOT NULL,
                phase TEXT NOT NULL CHECK(phase='post_may_have_started'),
                delivery INTEGER NOT NULL CHECK(delivery BETWEEN 0 AND 2),
                observation INTEGER NOT NULL CHECK(observation BETWEEN 0 AND 3));
            CREATE TABLE proof_claims (
                mint TEXT NOT NULL, y TEXT NOT NULL, keyset TEXT NOT NULL,
                amount INTEGER NOT NULL CHECK(amount>0),
                operation TEXT NOT NULL REFERENCES sends(operation),
                PRIMARY KEY(mint,y));
            PRAGMA user_version=2;";

#[derive(Debug)]
pub enum Error {
    Sql(rusqlite::Error),
    Io(std::io::Error),
    Invalid,
    Conflict,
    CapExceeded,
    Schema,
    ClockRollback,
}
impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sql(e)
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
#[cfg(test)]
#[path = "../tests/support/terminal_boundary.rs"]
mod terminal_boundary;

pub type Result<T> = std::result::Result<T, Error>;
pub struct BudgetStore {
    pub(crate) db: Connection,
}
pub(crate) fn number(n: u64) -> Result<i64> {
    i64::try_from(n).map_err(|_| Error::Invalid)
}
pub(crate) fn label(s: &str) -> Result<()> {
    if s.is_empty()
        || s.len() > 128
        || !s
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_:./".contains(&c))
    {
        return Err(Error::Invalid);
    }
    Ok(())
}
impl BudgetStore {
    /// Explicit enrollment only. Never overwrites or repairs an existing ledger.
    /// Parent must already exist on a private, local, durable filesystem.
    pub fn create(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let file = opts.open(path)?;
        file.sync_all()?;
        let mut db = Self::connect(path)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(SCHEMA)?;
        tx.commit()?;
        File::open(path)?.sync_all()?;
        File::open(
            path.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?
        .sync_all()?;
        Self::open(path)
    }
    fn connect(path: &Path) -> Result<Connection> {
        let db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        db.busy_timeout(Duration::from_secs(5))?;
        db.pragma_update(None, "foreign_keys", true)?;
        db.pragma_update(None, "synchronous", "FULL")?;
        let mode: String = db.query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0))?;
        if mode != "delete" {
            return Err(Error::Schema);
        }
        Ok(db)
    }
    /// Existing-only: missing, corrupt, unreadable or unsupported databases fail closed.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let db = Self::connect(path.as_ref())?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let integrity: String = db.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        if version != 2 || integrity != "ok" {
            return Err(Error::Schema);
        }
        db.query_row("SELECT high_day FROM metadata WHERE singleton=1", [], |r| {
            r.get::<_, i64>(0)
        })?;
        db.prepare("SELECT id,unit,day,face,fee,cap,total FROM reservations")?;
        db.prepare("SELECT id,reservation,kind,day,amount FROM operations")?;
        let violations: i64 =
            db.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })?;
        if violations != 0 {
            return Err(Error::Schema);
        }
        // Compare the complete schema to the versioned definition, including indexes.
        let reference = Connection::open_in_memory()?;
        reference.execute_batch(SCHEMA)?;
        fn schema(db: &Connection) -> Result<Vec<(String, String)>> {
            Ok(db
                .prepare("SELECT name,sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY name")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<std::result::Result<Vec<_>, _>>()?)
        }
        if schema(&db)? != schema(&reference)? {
            return Err(Error::Schema);
        }
        let invalid: i64 = db.query_row("SELECT
            (SELECT count(*) FROM reservations r WHERE
                NOT EXISTS(SELECT 1 FROM operations o WHERE o.id=r.id AND o.reservation=r.id
                    AND o.kind='reserve' AND o.day=r.day AND o.amount=r.total)) +
            (SELECT count(*) FROM operations o JOIN reservations r ON r.id=o.reservation WHERE
                o.day<r.day OR o.amount>r.total OR (o.kind='release' AND o.amount!=0) OR
                (o.kind='reserve' AND (o.id!=r.id OR o.day!=r.day OR o.amount!=r.total))) +
            (SELECT count(*) FROM operations WHERE day>(SELECT high_day FROM metadata WHERE singleton=1))",
            [], |r| r.get(0))?;
        if invalid != 0 {
            return Err(Error::Schema);
        }
        let invalid_send: i64 = db.query_row("SELECT count(*) FROM sends s JOIN reservations r ON r.id=s.operation WHERE
            EXISTS(SELECT 1 FROM operations o WHERE o.reservation=s.operation AND o.kind!='reserve') OR
            r.face != (SELECT COALESCE(SUM(amount),0) FROM proof_claims p WHERE p.operation=s.operation) OR
            EXISTS(SELECT 1 FROM proof_claims p WHERE p.operation=s.operation AND p.mint!=s.mint)", [], |r|r.get(0))?;
        if invalid_send != 0 {
            return Err(Error::Schema);
        }
        Ok(Self { db })
    }
    /// Today's settled debits plus outstanding reservations from EVERY day, per unit.
    pub fn usage(&self, unit: &str, day: u64) -> Result<u64> {
        label(unit)?;
        Self::usage_on(&self.db, unit, number(day)?)
    }
    fn usage_on(db: &Connection, unit: &str, day: i64) -> Result<u64> {
        let value: i64 = db.query_row("SELECT COALESCE(SUM(amount),0) FROM (
            SELECT r.total AS amount FROM reservations r WHERE r.unit=?1
              AND NOT EXISTS(SELECT 1 FROM operations o WHERE o.reservation=r.id AND o.kind!='reserve')
            UNION ALL SELECT o.amount FROM operations o JOIN reservations r ON r.id=o.reservation
              WHERE r.unit=?1 AND o.kind='settle' AND o.day=?2)", params![unit,day], |r| r.get(0))?;
        u64::try_from(value).map_err(|_| Error::Invalid)
    }
    fn day(db: &Connection, day: i64) -> Result<()> {
        let high: i64 =
            db.query_row("SELECT high_day FROM metadata WHERE singleton=1", [], |r| {
                r.get(0)
            })?;
        if day < high {
            return Err(Error::ClockRollback);
        }
        db.execute("UPDATE metadata SET high_day=?1 WHERE singleton=1", [day])?;
        Ok(())
    }
    /// Reserve full token face value plus ONLY additional client-side fee bounds.
    /// Same ID and identical parameters are a no-op; different parameters conflict.
    pub fn reserve(
        &mut self,
        id: &str,
        unit: &str,
        day: u64,
        face: u64,
        fee: u64,
        cap: u64,
    ) -> Result<()> {
        label(id)?;
        label(unit)?;
        let total = number(face.checked_add(fee).ok_or(Error::Invalid)?)?;
        let (day, face, fee, cap) = (number(day)?, number(face)?, number(fee)?, number(cap)?);
        if face == 0 {
            return Err(Error::Invalid);
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::reserve_on(&tx, id, unit, day, face, fee, cap, total)?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn reserve_on(
        tx: &Connection,
        id: &str,
        unit: &str,
        day: i64,
        face: i64,
        fee: i64,
        cap: i64,
        total: i64,
    ) -> Result<()> {
        let old = tx
            .query_row(
                "SELECT unit,day,face,fee,cap FROM reservations WHERE id=?1",
                [id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                    ))
                },
            )
            .optional()?;
        if let Some(old) = old {
            return if old == (unit.to_owned(), day, face, fee, cap) {
                Ok(())
            } else {
                Err(Error::Conflict)
            };
        }
        Self::day(&tx, day)?;
        let used = Self::usage_on(&tx, unit, day)?;
        if used.checked_add(total as u64).ok_or(Error::Invalid)? > cap as u64 {
            return Err(Error::CapExceeded);
        }
        tx.execute(
            "INSERT INTO reservations VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![id, unit, day, face, fee, cap, total],
        )?;
        tx.execute(
            "INSERT INTO operations VALUES(?1,?1,'reserve',?2,?3)",
            params![id, day, total],
        )?;
        Ok(())
    }
    /// Final settlement releases unused exposure and books actual debit on observation day.
    /// Caller must prove all remaining exposure resolved; partial/ambiguous outcomes stay reserved.
    pub fn settle(
        &mut self,
        operation: &str,
        reservation: &str,
        day: u64,
        actual: u64,
    ) -> Result<()> {
        self.finish(operation, reservation, day, actual, "settle")
    }
    /// Only for proven zero-debit cancellation/recovery; not a refusal/timeout handler.
    pub fn release(&mut self, operation: &str, reservation: &str, day: u64) -> Result<()> {
        self.finish(operation, reservation, day, 0, "release")
    }
    fn finish(
        &mut self,
        operation: &str,
        reservation: &str,
        day: u64,
        actual: u64,
        kind: &str,
    ) -> Result<()> {
        label(operation)?;
        label(reservation)?;
        let (day, actual) = (number(day)?, number(actual)?);
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Serialize ownership validation with send_once and every terminal replay/write.
        // A pre-transaction read could miss a send committed while waiting for this lock.
        // Send-owned exposure cannot be resolved by the accounting-only caller seam.
        if tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sends WHERE operation=?1)",
            [reservation],
            |r| r.get::<_, bool>(0),
        )? {
            return Err(Error::Conflict);
        }
        let old = tx
            .query_row(
                "SELECT reservation,kind,day,amount FROM operations WHERE id=?1",
                [operation],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()?;
        if let Some(old) = old {
            return if old == (reservation.to_owned(), kind.to_owned(), day, actual) {
                Ok(())
            } else {
                Err(Error::Conflict)
            };
        }
        Self::day(&tx, day)?;
        let total: i64 = tx.query_row(
            "SELECT total FROM reservations WHERE id=?1",
            [reservation],
            |r| r.get(0),
        )?;
        if actual > total {
            return Err(Error::Invalid);
        }
        tx.execute(
            "INSERT INTO operations VALUES(?1,?2,?3,?4,?5)",
            params![operation, reservation, kind, day, actual],
        )?;
        tx.commit()?;
        Ok(())
    }
}
