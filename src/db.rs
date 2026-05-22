use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags, params, params_from_iter};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct RunRow {
    pub id: i64,
    pub label: Option<String>,
    pub source_type: String,
    pub source_path: String,
    pub imported_at: String,
    pub total_calls: i64,
    pub unique_ops: i64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct TotalsRow {
    pub unique_ops: i64,
    pub call_count: i64,
    pub total_device_time_us: f64,
    pub total_free_time_us: f64,
    pub total_time_us: f64,
    pub avg_occupancy_pct: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SummaryRow {
    pub run_id: i64,
    pub first_call_order: i64,
    pub category: String,
    pub op_name: String,
    pub call_count: i64,
    pub total_device_time_us: f64,
    pub total_free_time_us: f64,
    pub total_time_us: f64,
    pub avg_device_time_us: f64,
    pub avg_free_time_us: f64,
    pub avg_time_us: f64,
    pub avg_occupancy_pct: Option<f64>,
    pub min_occupancy_pct: Option<f64>,
    pub max_occupancy_pct: Option<f64>,
    pub min_device_time_us: f64,
    pub min_free_time_us: f64,
    pub min_time_us: f64,
    pub max_device_time_us: f64,
    pub max_free_time_us: f64,
    pub max_time_us: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CallRow {
    pub run_id: i64,
    pub call_order: i64,
    pub op_call_index: i64,
    pub category: String,
    pub op_name: String,
    pub device_time_us: f64,
    pub occupancy_pct: Option<f64>,
    pub free_time_us: f64,
    pub total_time_us: f64,
    pub start_ts_us: Option<f64>,
    pub end_ts_us: Option<f64>,
    pub device: Option<String>,
    pub stream: Option<String>,
    pub pid: Option<String>,
    pub tid: Option<String>,
    pub correlation: Option<String>,
    pub external_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortSpec {
    pub key: &'static str,
    pub label: &'static str,
    pub column: &'static str,
    pub ascending: bool,
}

impl SortSpec {
    pub const ALL: [SortSpec; 8] = [
        SortSpec::new("device", "Device total", "total_device_time_us", false),
        SortSpec::new("free", "Free total", "total_free_time_us", false),
        SortSpec::new("total", "Combined total", "total_time_us", false),
        SortSpec::new("count", "Call count", "call_count", false),
        SortSpec::new("avg_device", "Device avg", "avg_device_time_us", false),
        SortSpec::new("avg_free", "Free avg", "avg_free_time_us", false),
        SortSpec::new("occupancy", "Occupancy avg", "avg_occupancy_pct", false),
        SortSpec::new("first", "First call", "first_call_order", true),
    ];

    pub const fn new(
        key: &'static str,
        label: &'static str,
        column: &'static str,
        ascending: bool,
    ) -> Self {
        Self {
            key,
            label,
            column,
            ascending,
        }
    }

    pub fn from_key(key: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|sort| sort.key == key)
            .unwrap_or(Self::ALL[0])
    }

    pub fn next(self) -> Self {
        let idx = Self::ALL
            .iter()
            .position(|sort| sort.key == self.key)
            .unwrap_or(0);
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }

    pub fn order_sql(self) -> &'static str {
        if self.ascending { "ASC" } else { "DESC" }
    }
}

pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open_readonly(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("open sqlite db {}", path.display()))?;
        conn.execute_batch("PRAGMA query_only = ON; PRAGMA foreign_keys = ON;")?;
        Ok(Self { conn })
    }

    pub fn open_readwrite(path: &Path) -> Result<Self> {
        let conn =
            Connection::open(path).with_context(|| format!("open sqlite db {}", path.display()))?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        Ok(Self { conn })
    }

    pub fn runs(&self) -> Result<Vec<RunRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, label, source_type, source_path, imported_at, total_calls, unique_ops \
             FROM runs ORDER BY id DESC",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(RunRow {
                    id: row.get("id")?,
                    label: row.get("label")?,
                    source_type: row.get("source_type")?,
                    source_path: row.get("source_path")?,
                    imported_at: row.get("imported_at")?,
                    total_calls: row.get("total_calls")?,
                    unique_ops: row.get("unique_ops")?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn latest_run_id(&self) -> Result<i64> {
        let id =
            self.conn
                .query_row("SELECT id FROM runs ORDER BY id DESC LIMIT 1", [], |row| {
                    row.get(0)
                })?;
        Ok(id)
    }

    pub fn totals(&self, run_id: i64) -> Result<TotalsRow> {
        let totals = self.conn.query_row(
            "SELECT COUNT(*) AS unique_ops, COALESCE(SUM(call_count), 0) AS call_count, \
                    COALESCE(SUM(total_device_time_us), 0) AS total_device_time_us, \
                    COALESCE(SUM(total_free_time_us), 0) AS total_free_time_us, \
                    COALESCE(SUM(total_time_us), 0) AS total_time_us, \
                    SUM(CASE WHEN avg_occupancy_pct IS NOT NULL THEN avg_occupancy_pct * call_count ELSE 0 END) \
                      / NULLIF(SUM(CASE WHEN avg_occupancy_pct IS NOT NULL THEN call_count ELSE 0 END), 0) AS avg_occupancy_pct \
             FROM op_summary WHERE run_id = ?",
            params![run_id],
            |row| {
                Ok(TotalsRow {
                    unique_ops: row.get("unique_ops")?,
                    call_count: row.get("call_count")?,
                    total_device_time_us: row.get("total_device_time_us")?,
                    total_free_time_us: row.get("total_free_time_us")?,
                    total_time_us: row.get("total_time_us")?,
                    avg_occupancy_pct: row.get("avg_occupancy_pct")?,
                })
            },
        )?;
        Ok(totals)
    }

    pub fn summary(
        &self,
        run_id: i64,
        q: Option<&str>,
        sort: SortSpec,
        limit: i64,
    ) -> Result<Vec<SummaryRow>> {
        let mut sql = format!(
            "SELECT run_id, first_call_order, category, op_name, call_count, \
                    total_device_time_us, total_free_time_us, total_time_us, \
                    avg_device_time_us, avg_free_time_us, avg_time_us, \
                    avg_occupancy_pct, min_occupancy_pct, max_occupancy_pct, \
                    min_device_time_us, min_free_time_us, min_time_us, \
                    max_device_time_us, max_free_time_us, max_time_us \
             FROM op_summary WHERE run_id = ?"
        );
        let mut values = vec![Value::Integer(run_id)];
        if let Some(q) = q.filter(|value| !value.is_empty()) {
            sql.push_str(" AND op_name LIKE ?");
            values.push(Value::Text(format!("%{q}%")));
        }
        sql.push_str(&format!(
            " ORDER BY {} {} LIMIT ?",
            sort.column,
            sort.order_sql()
        ));
        values.push(Value::Integer(limit.max(1)));

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_from_iter(values.iter()), map_summary_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn calls(
        &self,
        run_id: i64,
        q: Option<&str>,
        op: Option<&str>,
        call_order: Option<i64>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<CallRow>> {
        let mut sql = String::from(
            "SELECT run_id, call_order, op_call_index, category, op_name, device_time_us, \
                    occupancy_pct, free_time_us, total_time_us, start_ts_us, end_ts_us, \
                    device, stream, pid, tid, correlation, external_id \
             FROM gpu_calls WHERE run_id = ?",
        );
        let mut values = vec![Value::Integer(run_id)];
        if let Some(q) = q.filter(|value| !value.is_empty()) {
            sql.push_str(" AND op_name LIKE ?");
            values.push(Value::Text(format!("%{q}%")));
        }
        if let Some(op) = op.filter(|value| !value.is_empty()) {
            sql.push_str(" AND op_name = ?");
            values.push(Value::Text(op.to_owned()));
        }
        if let Some(order) = call_order {
            sql.push_str(" AND call_order = ?");
            values.push(Value::Integer(order));
        }
        sql.push_str(" ORDER BY call_order ASC LIMIT ? OFFSET ?");
        values.push(Value::Integer(limit.max(1)));
        values.push(Value::Integer(offset.max(0)));

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_from_iter(values.iter()), map_call_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn call_context(&self, run_id: i64, call_order: i64, radius: i64) -> Result<Vec<CallRow>> {
        let lo = 1.max(call_order - radius.max(1));
        let hi = call_order + radius.max(1);
        let mut stmt = self.conn.prepare(
            "SELECT run_id, call_order, op_call_index, category, op_name, device_time_us, \
                    occupancy_pct, free_time_us, total_time_us, start_ts_us, end_ts_us, \
                    device, stream, pid, tid, correlation, external_id \
             FROM gpu_calls WHERE run_id = ? AND call_order BETWEEN ? AND ? \
             ORDER BY call_order ASC",
        )?;
        let rows = stmt
            .query_map(params![run_id, lo, hi], map_call_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn delete_run(path: &Path, run_id: i64) -> Result<()> {
        let db = Self::open_readwrite(path)?;
        db.conn
            .execute("DELETE FROM runs WHERE id = ?", params![run_id])?;
        Ok(())
    }
}

fn map_summary_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SummaryRow> {
    Ok(SummaryRow {
        run_id: row.get("run_id")?,
        first_call_order: row.get("first_call_order")?,
        category: row.get("category")?,
        op_name: row.get("op_name")?,
        call_count: row.get("call_count")?,
        total_device_time_us: row.get("total_device_time_us")?,
        total_free_time_us: row.get("total_free_time_us")?,
        total_time_us: row.get("total_time_us")?,
        avg_device_time_us: row.get("avg_device_time_us")?,
        avg_free_time_us: row.get("avg_free_time_us")?,
        avg_time_us: row.get("avg_time_us")?,
        avg_occupancy_pct: row.get("avg_occupancy_pct")?,
        min_occupancy_pct: row.get("min_occupancy_pct")?,
        max_occupancy_pct: row.get("max_occupancy_pct")?,
        min_device_time_us: row.get("min_device_time_us")?,
        min_free_time_us: row.get("min_free_time_us")?,
        min_time_us: row.get("min_time_us")?,
        max_device_time_us: row.get("max_device_time_us")?,
        max_free_time_us: row.get("max_free_time_us")?,
        max_time_us: row.get("max_time_us")?,
    })
}

fn map_call_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CallRow> {
    Ok(CallRow {
        run_id: row.get("run_id")?,
        call_order: row.get("call_order")?,
        op_call_index: row.get("op_call_index")?,
        category: row.get("category")?,
        op_name: row.get("op_name")?,
        device_time_us: row.get("device_time_us")?,
        occupancy_pct: row.get("occupancy_pct")?,
        free_time_us: row.get("free_time_us")?,
        total_time_us: row.get("total_time_us")?,
        start_ts_us: row.get("start_ts_us")?,
        end_ts_us: row.get("end_ts_us")?,
        device: row.get("device")?,
        stream: row.get("stream")?,
        pid: row.get("pid")?,
        tid: row.get("tid")?,
        correlation: row.get("correlation")?,
        external_id: row.get("external_id")?,
    })
}
