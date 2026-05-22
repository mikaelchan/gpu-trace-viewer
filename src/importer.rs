use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::Local;
use flate2::read::GzDecoder;
use regex::Regex;
use rusqlite::{Transaction, params};
use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};

use crate::db::Db;

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_CATEGORIES: &str = "kernel";
const CALL_COLUMNS: &[&str] = &[
    "run_id",
    "call_order",
    "op_call_index",
    "category",
    "op_name",
    "device_time_us",
    "occupancy_pct",
    "blocks_per_sm",
    "warps_per_sm",
    "shared_memory",
    "grid",
    "block",
    "free_time_us",
    "total_time_us",
    "start_ts_us",
    "end_ts_us",
    "device",
    "stream",
    "pid",
    "tid",
    "correlation",
    "external_id",
];
const SUMMARY_COLUMNS: &[&str] = &[
    "run_id",
    "first_call_order",
    "category",
    "op_name",
    "call_count",
    "total_device_time_us",
    "total_free_time_us",
    "total_time_us",
    "avg_device_time_us",
    "avg_free_time_us",
    "avg_time_us",
    "avg_occupancy_pct",
    "min_occupancy_pct",
    "max_occupancy_pct",
    "min_device_time_us",
    "min_free_time_us",
    "min_time_us",
    "max_device_time_us",
    "max_free_time_us",
    "max_time_us",
];

#[derive(Debug, Clone)]
pub struct ImportTraceOptions {
    pub db_path: PathBuf,
    pub trace_path: PathBuf,
    pub label: Option<String>,
    pub categories: String,
    pub all_categories: bool,
    pub include_name: Option<String>,
    pub exclude_name: Option<String>,
    pub min_device_time_us: f64,
    pub batch_size: usize,
    pub progress_interval: i64,
}

impl ImportTraceOptions {
    pub fn new(db_path: PathBuf, trace_path: PathBuf) -> Self {
        Self {
            db_path,
            trace_path,
            label: None,
            categories: DEFAULT_CATEGORIES.to_string(),
            all_categories: false,
            include_name: None,
            exclude_name: None,
            min_device_time_us: 0.0,
            batch_size: 1000,
            progress_interval: 100_000,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ImportCsvOptions {
    pub db_path: PathBuf,
    pub csv_path: PathBuf,
    pub label: Option<String>,
    pub batch_size: usize,
    pub progress_interval: i64,
}

impl ImportCsvOptions {
    pub fn new(db_path: PathBuf, csv_path: PathBuf) -> Self {
        Self {
            db_path,
            csv_path,
            label: None,
            batch_size: 1000,
            progress_interval: 100_000,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ImportResult {
    pub db_path: PathBuf,
    pub run_id: i64,
    pub gpu_calls: i64,
    pub unique_ops: usize,
}

#[derive(Debug, Clone)]
struct DeviceEvent {
    category: String,
    op_name: String,
    device_time_us: f64,
    occupancy_pct: Option<f64>,
    blocks_per_sm: Option<f64>,
    warps_per_sm: Option<f64>,
    shared_memory: Option<f64>,
    grid: Option<String>,
    block: Option<String>,
    start_us: f64,
    end_us: f64,
    device: String,
    stream: String,
    pid: String,
    tid: String,
    correlation: String,
    external_id: String,
}

#[derive(Debug, Clone)]
struct CallRecord {
    run_id: i64,
    call_order: i64,
    op_call_index: i64,
    category: String,
    op_name: String,
    device_time_us: f64,
    occupancy_pct: Option<f64>,
    blocks_per_sm: Option<f64>,
    warps_per_sm: Option<f64>,
    shared_memory: Option<f64>,
    grid: Option<String>,
    block: Option<String>,
    free_time_us: f64,
    total_time_us: f64,
    start_ts_us: Option<f64>,
    end_ts_us: Option<f64>,
    device: String,
    stream: String,
    pid: String,
    tid: String,
    correlation: String,
    external_id: String,
}

#[derive(Debug, Clone)]
struct SummaryKey {
    category: String,
    op_name: String,
}

impl PartialEq for SummaryKey {
    fn eq(&self, other: &Self) -> bool {
        self.category == other.category && self.op_name == other.op_name
    }
}

impl Eq for SummaryKey {}

impl Hash for SummaryKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.category.hash(state);
        self.op_name.hash(state);
    }
}

#[derive(Debug, Clone)]
struct SummaryAgg {
    first_call_order: i64,
    category: String,
    op_name: String,
    call_count: i64,
    total_device_time_us: f64,
    total_free_time_us: f64,
    total_time_us: f64,
    min_device_time_us: f64,
    min_free_time_us: f64,
    min_time_us: f64,
    max_device_time_us: f64,
    max_free_time_us: f64,
    max_time_us: f64,
    total_occupancy_pct: f64,
    occupancy_count: i64,
    min_occupancy_pct: f64,
    max_occupancy_pct: f64,
}

impl SummaryAgg {
    fn new(first_call_order: i64, category: &str, op_name: &str) -> Self {
        Self {
            first_call_order,
            category: category.to_string(),
            op_name: op_name.to_string(),
            call_count: 0,
            total_device_time_us: 0.0,
            total_free_time_us: 0.0,
            total_time_us: 0.0,
            min_device_time_us: f64::INFINITY,
            min_free_time_us: f64::INFINITY,
            min_time_us: f64::INFINITY,
            max_device_time_us: 0.0,
            max_free_time_us: 0.0,
            max_time_us: 0.0,
            total_occupancy_pct: 0.0,
            occupancy_count: 0,
            min_occupancy_pct: f64::INFINITY,
            max_occupancy_pct: 0.0,
        }
    }

    fn add(&mut self, device_time_us: f64, free_time_us: f64, occupancy_pct: Option<f64>) {
        let total_time_us = device_time_us + free_time_us;
        self.call_count += 1;
        self.total_device_time_us += device_time_us;
        self.total_free_time_us += free_time_us;
        self.total_time_us += total_time_us;
        self.min_device_time_us = self.min_device_time_us.min(device_time_us);
        self.min_free_time_us = self.min_free_time_us.min(free_time_us);
        self.min_time_us = self.min_time_us.min(total_time_us);
        self.max_device_time_us = self.max_device_time_us.max(device_time_us);
        self.max_free_time_us = self.max_free_time_us.max(free_time_us);
        self.max_time_us = self.max_time_us.max(total_time_us);
        if let Some(value) = occupancy_pct {
            self.total_occupancy_pct += value;
            self.occupancy_count += 1;
            self.min_occupancy_pct = self.min_occupancy_pct.min(value);
            self.max_occupancy_pct = self.max_occupancy_pct.max(value);
        }
    }

    fn avg(&self, total_us: f64) -> f64 {
        if self.call_count == 0 {
            0.0
        } else {
            total_us / self.call_count as f64
        }
    }

    fn avg_occupancy_pct(&self) -> Option<f64> {
        if self.occupancy_count == 0 {
            None
        } else {
            Some(self.total_occupancy_pct / self.occupancy_count as f64)
        }
    }

    fn min_occupancy_pct(&self) -> Option<f64> {
        (self.occupancy_count > 0).then_some(self.min_occupancy_pct)
    }

    fn max_occupancy_pct(&self) -> Option<f64> {
        (self.occupancy_count > 0).then_some(self.max_occupancy_pct)
    }
}

pub fn import_trace(options: ImportTraceOptions) -> Result<ImportResult> {
    let trace_path = options.trace_path.expand_user();
    if !trace_path.exists() {
        bail!("trace file not found: {}", trace_path.display());
    }
    let categories = parse_categories(&options.categories, options.all_categories)?;
    let category_text = category_text(&categories);
    let include_name = compile_regex(options.include_name.as_deref(), "--include-name")?;
    let exclude_name = compile_regex(options.exclude_name.as_deref(), "--exclude-name")?;

    let mut db = Db::open_readwrite(&options.db_path)?;
    db.tune_for_import()?;
    let tx = db.conn.transaction()?;
    let run_id = insert_run(
        &tx,
        options.label.as_deref(),
        "trace",
        &trace_path,
        &category_text,
        options.include_name.as_deref(),
        options.exclude_name.as_deref(),
        options.min_device_time_us,
    )?;

    let mut summary = HashMap::new();
    let mut summary_order = Vec::new();
    let mut last_stream_end_us = HashMap::<(String, String), f64>::new();
    let mut batch = Vec::with_capacity(options.batch_size);
    let mut matched_calls = 0_i64;
    let mut decoded_events = 0_i64;

    let reader = open_trace_reader(&trace_path)?;
    stream_trace_events(reader, |event| {
        decoded_events += 1;
        let Some(device_event) = parse_device_event(event, categories.as_ref()) else {
            return Ok(());
        };

        let stream_key = (device_event.device.clone(), device_event.stream.clone());
        let free_time_us = last_stream_end_us
            .get(&stream_key)
            .map(|previous| (device_event.start_us - previous).max(0.0))
            .unwrap_or(0.0);
        let current_end = last_stream_end_us
            .get(&stream_key)
            .copied()
            .unwrap_or(f64::NEG_INFINITY)
            .max(device_event.end_us);
        last_stream_end_us.insert(stream_key, current_end);

        if device_event.device_time_us < options.min_device_time_us {
            return Ok(());
        }
        if !selected_name(
            &device_event.op_name,
            include_name.as_ref(),
            exclude_name.as_ref(),
        ) {
            return Ok(());
        }

        matched_calls += 1;
        let total_time_us = device_event.device_time_us + free_time_us;
        let op_call_index = add_summary(
            &mut summary,
            &mut summary_order,
            matched_calls,
            &device_event.category,
            &device_event.op_name,
            device_event.device_time_us,
            free_time_us,
            device_event.occupancy_pct,
        );
        batch.push(CallRecord {
            run_id,
            call_order: matched_calls,
            op_call_index,
            category: device_event.category,
            op_name: device_event.op_name,
            device_time_us: device_event.device_time_us,
            occupancy_pct: device_event.occupancy_pct,
            blocks_per_sm: device_event.blocks_per_sm,
            warps_per_sm: device_event.warps_per_sm,
            shared_memory: device_event.shared_memory,
            grid: device_event.grid,
            block: device_event.block,
            free_time_us,
            total_time_us,
            start_ts_us: Some(device_event.start_us),
            end_ts_us: Some(device_event.end_us),
            device: device_event.device,
            stream: device_event.stream,
            pid: device_event.pid,
            tid: device_event.tid,
            correlation: device_event.correlation,
            external_id: device_event.external_id,
        });

        if batch.len() >= options.batch_size.max(1) {
            flush_calls(&tx, &mut batch)?;
        }
        if options.progress_interval > 0 && matched_calls % options.progress_interval == 0 {
            eprintln!(
                "run {run_id}: imported {matched_calls} GPU calls, decoded {decoded_events} events"
            );
        }
        Ok(())
    })?;

    flush_calls(&tx, &mut batch)?;
    finalize_run(&tx, run_id, matched_calls, &summary, &summary_order)?;
    tx.commit()?;

    Ok(ImportResult {
        db_path: options.db_path,
        run_id,
        gpu_calls: matched_calls,
        unique_ops: summary_order.len(),
    })
}

pub fn import_csv(options: ImportCsvOptions) -> Result<ImportResult> {
    let csv_path = options.csv_path.expand_user();
    if !csv_path.exists() {
        bail!("CSV file not found: {}", csv_path.display());
    }

    let mut db = Db::open_readwrite(&options.db_path)?;
    db.tune_for_import()?;
    let tx = db.conn.transaction()?;
    let run_id = insert_run(
        &tx,
        options.label.as_deref(),
        "calls_csv",
        &csv_path,
        "from_csv",
        None,
        None,
        0.0,
    )?;

    let mut summary = HashMap::new();
    let mut summary_order = Vec::new();
    let mut batch = Vec::with_capacity(options.batch_size);
    let mut total_rows = 0_i64;
    let mut reader = csv::Reader::from_path(&csv_path)
        .with_context(|| format!("open CSV {}", csv_path.display()))?;

    for row in reader.deserialize::<HashMap<String, String>>() {
        let row = row?;
        total_rows += 1;
        let call_order = parse_float(&row, &["call_order"], total_rows as f64) as i64;
        let category = parse_text(&row, &["category"], "kernel");
        let op_name = parse_text(&row, &["op_name", "name"], "");
        let device_time_us = parse_float(&row, &["device_time_us", "duration_us"], 0.0);
        let free_time_us = parse_float(&row, &["free_time_us"], 0.0);
        let total_time_us = parse_float(&row, &["total_time_us"], device_time_us + free_time_us);
        let occupancy_pct = parse_optional_float(&row, &["occupancy_pct", "occupancy"]);
        let blocks_per_sm = parse_optional_float(&row, &["blocks_per_sm", "blocks per SM"]);
        let warps_per_sm = parse_optional_float(&row, &["warps_per_sm", "warps per SM"]);
        let shared_memory = parse_optional_float(&row, &["shared_memory", "shared memory"]);
        let start_ts_us = parse_float(&row, &["start_ts_us", "ts_us"], 0.0);
        let end_ts_us = parse_float(&row, &["end_ts_us"], start_ts_us + device_time_us);
        let op_call_index = add_summary(
            &mut summary,
            &mut summary_order,
            call_order,
            &category,
            &op_name,
            device_time_us,
            free_time_us,
            occupancy_pct,
        );

        batch.push(CallRecord {
            run_id,
            call_order,
            op_call_index,
            category,
            op_name,
            device_time_us,
            occupancy_pct,
            blocks_per_sm,
            warps_per_sm,
            shared_memory,
            grid: parse_optional_text(&row, &["grid"]),
            block: parse_optional_text(&row, &["block"]),
            free_time_us,
            total_time_us,
            start_ts_us: Some(start_ts_us),
            end_ts_us: Some(end_ts_us),
            device: parse_text(&row, &["device"], ""),
            stream: parse_text(&row, &["stream"], ""),
            pid: parse_text(&row, &["pid"], ""),
            tid: parse_text(&row, &["tid"], ""),
            correlation: parse_text(&row, &["correlation"], ""),
            external_id: parse_text(&row, &["external_id"], ""),
        });

        if batch.len() >= options.batch_size.max(1) {
            flush_calls(&tx, &mut batch)?;
        }
        if options.progress_interval > 0 && total_rows % options.progress_interval == 0 {
            eprintln!("run {run_id}: imported {total_rows} CSV rows");
        }
    }

    flush_calls(&tx, &mut batch)?;
    finalize_run(&tx, run_id, total_rows, &summary, &summary_order)?;
    tx.commit()?;

    Ok(ImportResult {
        db_path: options.db_path,
        run_id,
        gpu_calls: total_rows,
        unique_ops: summary_order.len(),
    })
}

fn parse_categories(raw_categories: &str, all_categories: bool) -> Result<Option<HashSet<String>>> {
    if all_categories {
        return Ok(None);
    }
    let categories = raw_categories
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect::<HashSet<_>>();
    if categories.is_empty() {
        bail!("--categories must not be empty unless --all-categories is used");
    }
    Ok(Some(categories))
}

fn category_text(categories: &Option<HashSet<String>>) -> String {
    match categories {
        None => "*".to_string(),
        Some(categories) => {
            let mut values = categories.iter().cloned().collect::<Vec<_>>();
            values.sort();
            values.join(",")
        }
    }
}

fn compile_regex(raw_pattern: Option<&str>, flag_name: &str) -> Result<Option<Regex>> {
    raw_pattern
        .filter(|value| !value.is_empty())
        .map(|pattern| Regex::new(pattern).with_context(|| format!("{flag_name} is invalid")))
        .transpose()
}

fn selected_name(
    op_name: &str,
    include_name: Option<&Regex>,
    exclude_name: Option<&Regex>,
) -> bool {
    if include_name.is_some_and(|regex| !regex.is_match(op_name)) {
        return false;
    }
    if exclude_name.is_some_and(|regex| regex.is_match(op_name)) {
        return false;
    }
    true
}

fn open_trace_reader(path: &Path) -> Result<Box<dyn Read>> {
    let file = File::open(path).with_context(|| format!("open trace {}", path.display()))?;
    if path.extension().and_then(|value| value.to_str()) == Some("gz") {
        Ok(Box::new(GzDecoder::new(file)))
    } else {
        Ok(Box::new(file))
    }
}

fn stream_trace_events<R, F>(reader: R, mut on_event: F) -> Result<()>
where
    R: Read,
    F: FnMut(Value) -> Result<()>,
{
    let mut found_trace_events = false;
    let seed = TraceRootSeed {
        on_event: &mut on_event,
        found_trace_events: &mut found_trace_events,
    };
    let mut deserializer = serde_json::Deserializer::from_reader(BufReader::new(reader));
    seed.deserialize(&mut deserializer)?;
    if !found_trace_events {
        bail!("trace does not contain a traceEvents array");
    }
    Ok(())
}

struct TraceRootSeed<'a, F> {
    on_event: &'a mut F,
    found_trace_events: &'a mut bool,
}

impl<'de, F> DeserializeSeed<'de> for TraceRootSeed<'_, F>
where
    F: FnMut(Value) -> Result<()>,
{
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        deserializer.deserialize_map(TraceRootVisitor {
            on_event: self.on_event,
            found_trace_events: self.found_trace_events,
        })
    }
}

struct TraceRootVisitor<'a, F> {
    on_event: &'a mut F,
    found_trace_events: &'a mut bool,
}

impl<'de, F> Visitor<'de> for TraceRootVisitor<'_, F>
where
    F: FnMut(Value) -> Result<()>,
{
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a trace root object")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        while let Some(key) = map.next_key::<String>()? {
            if key == "traceEvents" {
                *self.found_trace_events = true;
                map.next_value_seed(TraceEventsSeed {
                    on_event: self.on_event,
                })?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(())
    }
}

struct TraceEventsSeed<'a, F> {
    on_event: &'a mut F,
}

impl<'de, F> DeserializeSeed<'de> for TraceEventsSeed<'_, F>
where
    F: FnMut(Value) -> Result<()>,
{
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        deserializer.deserialize_seq(TraceEventsVisitor {
            on_event: self.on_event,
        })
    }
}

struct TraceEventsVisitor<'a, F> {
    on_event: &'a mut F,
}

impl<'de, F> Visitor<'de> for TraceEventsVisitor<'_, F>
where
    F: FnMut(Value) -> Result<()>,
{
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the traceEvents array")
    }

    fn visit_seq<A>(self, mut seq: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while let Some(event) = seq.next_element::<Value>()? {
            (self.on_event)(event).map_err(de::Error::custom)?;
        }
        Ok(())
    }
}

fn parse_device_event(event: Value, categories: Option<&HashSet<String>>) -> Option<DeviceEvent> {
    let object = event.as_object()?;
    if object.get("ph").and_then(Value::as_str) != Some("X") {
        return None;
    }
    let device_time_us = object.get("dur")?.as_f64()?;
    let start_us = object.get("ts")?.as_f64()?;
    let name = object.get("name")?;
    let category = trace_value_to_string(object.get("cat")).unwrap_or_default();
    if categories.is_some_and(|categories| !categories.contains(&category)) {
        return None;
    }

    let args = object.get("args").and_then(Value::as_object);
    let occupancy_pct = parse_occupancy_pct(args);
    let blocks_per_sm = parse_arg_float(args, "blocks per SM");
    let warps_per_sm = parse_arg_float(args, "warps per SM");
    let shared_memory = parse_arg_float(args, "shared memory");
    let device = trace_value_to_string(arg_or(args, object, "device", "pid")).unwrap_or_default();
    let stream = trace_value_to_string(arg_or(args, object, "stream", "tid")).unwrap_or_default();

    Some(DeviceEvent {
        category,
        op_name: trace_value_to_string(Some(name)).unwrap_or_default(),
        device_time_us,
        occupancy_pct,
        blocks_per_sm,
        warps_per_sm,
        shared_memory,
        grid: args
            .and_then(|args| args.get("grid"))
            .and_then(trace_dim_to_string),
        block: args
            .and_then(|args| args.get("block"))
            .and_then(trace_dim_to_string),
        start_us,
        end_us: start_us + device_time_us,
        device,
        stream,
        pid: trace_value_to_string(object.get("pid")).unwrap_or_default(),
        tid: trace_value_to_string(object.get("tid")).unwrap_or_default(),
        correlation: trace_value_to_string(args.and_then(|args| args.get("correlation")))
            .unwrap_or_default(),
        external_id: trace_value_to_string(args.and_then(|args| args.get("External id")))
            .unwrap_or_default(),
    })
}

fn arg_or<'a>(
    args: Option<&'a Map<String, Value>>,
    object: &'a Map<String, Value>,
    arg_key: &str,
    object_key: &str,
) -> Option<&'a Value> {
    args.and_then(|args| args.get(arg_key))
        .or_else(|| object.get(object_key))
}

fn parse_occupancy_pct(args: Option<&Map<String, Value>>) -> Option<f64> {
    let args = args?;
    for key in [
        "est. achieved occupancy %",
        "occupancy_pct",
        "occupancy",
        "achieved_occupancy",
    ] {
        if let Some(value) = args.get(key) {
            if let Some(parsed) = value.as_f64() {
                return Some(parsed);
            }
            if let Some(parsed) = value.as_str().and_then(|value| value.parse::<f64>().ok()) {
                return Some(parsed);
            }
        }
    }
    None
}

fn parse_arg_float(args: Option<&Map<String, Value>>, key: &str) -> Option<f64> {
    let value = args?.get(key)?;
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|value| value.parse::<f64>().ok()))
}

fn trace_dim_to_string(value: &Value) -> Option<String> {
    if let Some(items) = value.as_array() {
        return Some(
            items
                .iter()
                .filter_map(|item| trace_value_to_string(Some(item)))
                .collect::<Vec<_>>()
                .join("x"),
        );
    }
    trace_value_to_string(Some(value)).filter(|value| !value.is_empty())
}

fn trace_value_to_string(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Null => Some(String::new()),
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        value => Some(value.to_string()),
    }
}

fn add_summary(
    summary: &mut HashMap<SummaryKey, SummaryAgg>,
    summary_order: &mut Vec<SummaryKey>,
    call_order: i64,
    category: &str,
    op_name: &str,
    device_time_us: f64,
    free_time_us: f64,
    occupancy_pct: Option<f64>,
) -> i64 {
    let key = SummaryKey {
        category: category.to_string(),
        op_name: op_name.to_string(),
    };
    if !summary.contains_key(&key) {
        summary.insert(key.clone(), SummaryAgg::new(call_order, category, op_name));
        summary_order.push(key.clone());
    }
    let agg = summary.get_mut(&key).expect("summary key inserted");
    agg.add(device_time_us, free_time_us, occupancy_pct);
    agg.call_count
}

fn insert_run(
    tx: &Transaction<'_>,
    label: Option<&str>,
    source_type: &str,
    source_path: &Path,
    categories: &str,
    include_name: Option<&str>,
    exclude_name: Option<&str>,
    min_device_time_us: f64,
) -> Result<i64> {
    tx.execute(
        "INSERT INTO runs (label, source_type, source_path, source_size_bytes, imported_at, app_version, categories, include_name, exclude_name, min_device_time_us) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            label,
            source_type,
            source_path.to_string_lossy().as_ref(),
            source_size(source_path),
            Local::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
            APP_VERSION,
            categories,
            include_name,
            exclude_name,
            min_device_time_us,
        ],
    )?;
    Ok(tx.last_insert_rowid())
}

fn source_size(path: &Path) -> Option<i64> {
    path.metadata()
        .ok()
        .and_then(|metadata| i64::try_from(metadata.len()).ok())
}

fn flush_calls(tx: &Transaction<'_>, batch: &mut Vec<CallRecord>) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    let placeholders = ["?"; CALL_COLUMNS.len()].join(",");
    let sql = format!(
        "INSERT INTO gpu_calls ({}) VALUES ({placeholders})",
        CALL_COLUMNS.join(",")
    );
    let mut stmt = tx.prepare(&sql)?;
    for row in batch.drain(..) {
        stmt.execute(params![
            row.run_id,
            row.call_order,
            row.op_call_index,
            row.category,
            row.op_name,
            row.device_time_us,
            row.occupancy_pct,
            row.blocks_per_sm,
            row.warps_per_sm,
            row.shared_memory,
            row.grid,
            row.block,
            row.free_time_us,
            row.total_time_us,
            row.start_ts_us,
            row.end_ts_us,
            row.device,
            row.stream,
            row.pid,
            row.tid,
            row.correlation,
            row.external_id,
        ])?;
    }
    Ok(())
}

fn finalize_run(
    tx: &Transaction<'_>,
    run_id: i64,
    total_calls: i64,
    summary: &HashMap<SummaryKey, SummaryAgg>,
    summary_order: &[SummaryKey],
) -> Result<()> {
    let placeholders = ["?"; SUMMARY_COLUMNS.len()].join(",");
    let sql = format!(
        "INSERT INTO op_summary ({}) VALUES ({placeholders})",
        SUMMARY_COLUMNS.join(",")
    );
    let mut stmt = tx.prepare(&sql)?;
    for key in summary_order {
        let row = summary
            .get(key)
            .expect("summary order references known key");
        stmt.execute(params![
            run_id,
            row.first_call_order,
            row.category,
            row.op_name,
            row.call_count,
            row.total_device_time_us,
            row.total_free_time_us,
            row.total_time_us,
            row.avg(row.total_device_time_us),
            row.avg(row.total_free_time_us),
            row.avg(row.total_time_us),
            row.avg_occupancy_pct(),
            row.min_occupancy_pct(),
            row.max_occupancy_pct(),
            row.min_device_time_us,
            row.min_free_time_us,
            row.min_time_us,
            row.max_device_time_us,
            row.max_free_time_us,
            row.max_time_us,
        ])?;
    }
    tx.execute(
        "UPDATE runs SET total_calls = ?, unique_ops = ? WHERE id = ?",
        params![total_calls, summary_order.len() as i64, run_id],
    )?;
    Ok(())
}

fn parse_float(row: &HashMap<String, String>, names: &[&str], default: f64) -> f64 {
    names
        .iter()
        .find_map(|name| row.get(*name))
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(default)
}

fn parse_optional_float(row: &HashMap<String, String>, names: &[&str]) -> Option<f64> {
    names
        .iter()
        .find_map(|name| row.get(*name))
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse::<f64>().ok())
}

fn parse_optional_text(row: &HashMap<String, String>, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| row.get(*name))
        .filter(|value| !value.is_empty())
        .cloned()
}

fn parse_text(row: &HashMap<String, String>, names: &[&str], default: &str) -> String {
    names
        .iter()
        .find_map(|name| row.get(*name))
        .filter(|value| !value.is_empty())
        .cloned()
        .unwrap_or_else(|| default.to_string())
}

trait ExpandUser {
    fn expand_user(&self) -> PathBuf;
}

impl ExpandUser for Path {
    fn expand_user(&self) -> PathBuf {
        let Some(raw) = self.to_str() else {
            return self.to_path_buf();
        };
        if raw == "~" {
            if let Some(home) = std::env::var_os("HOME") {
                return PathBuf::from(home);
            }
        }
        if let Some(rest) = raw.strip_prefix("~/") {
            if let Some(home) = std::env::var_os("HOME") {
                return PathBuf::from(home).join(rest);
            }
        }
        self.to_path_buf()
    }
}
