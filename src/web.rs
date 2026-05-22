use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;

use anyhow::{Context, Result, anyhow};
use serde::Serialize;
use serde_json::json;

use crate::db::{CallRow, Db, SortSpec, SummaryRow, TotalsRow};
use crate::importer::{ImportCsvOptions, ImportTraceOptions, import_csv, import_trace};

#[derive(Debug, Serialize)]
struct SummaryPayload {
    run_id: i64,
    totals: TotalsRow,
    rows: Vec<SummaryRow>,
}

#[derive(Debug, Serialize)]
struct CallsPayload {
    run_id: i64,
    rows: Vec<CallRow>,
}

#[derive(Debug, Serialize)]
struct ContextPayload {
    run_id: i64,
    call_order: i64,
    radius: i64,
    rows: Vec<CallRow>,
}

pub fn serve(db_path: PathBuf, host: &str, port: u16) -> Result<()> {
    if db_path.exists() {
        Db::open_readonly(&db_path)?;
    } else {
        Db::open_readwrite(&db_path)?;
    }
    let listener = TcpListener::bind((host, port))?;
    let addr = listener.local_addr()?;
    println!("serving {} at http://{}", db_path.display(), addr);

    for stream in listener.incoming() {
        let db_path = db_path.clone();
        match stream {
            Ok(stream) => {
                thread::spawn(move || {
                    if let Err(err) = handle_stream(stream, &db_path) {
                        eprintln!("request error: {err:#}");
                    }
                });
            }
            Err(err) => eprintln!("accept error: {err}"),
        }
    }
    Ok(())
}

fn handle_stream(mut stream: TcpStream, db_path: &Path) -> Result<()> {
    let mut buf = vec![0_u8; 128 * 1024];
    let n = stream.read(&mut buf)?;
    if n == 0 {
        return Ok(());
    }
    let req = String::from_utf8_lossy(&buf[..n]);
    let mut lines = req.lines();
    let first = lines.next().ok_or_else(|| anyhow!("empty request"))?;
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("");
    let uri = parts.next().unwrap_or("/");
    let (path, params) = parse_uri(uri);

    let response = match (method, path.as_str()) {
        ("GET", "/") => html_response(INDEX_HTML),
        ("GET", "/api/runs") => {
            json_response(&json!({ "runs": Db::open_readonly(db_path)?.runs()? }))?
        }
        ("GET", "/api/summary") => api_summary(db_path, &params)?,
        ("GET", "/api/calls") => api_calls(db_path, &params)?,
        ("GET", "/api/call-context") => api_call_context(db_path, &params)?,
        ("GET", "/download/summary.csv") => download_summary(db_path, &params)?,
        ("GET", "/download/calls.csv") => download_calls(db_path, &params)?,
        ("POST", "/api/import-trace") => api_import_trace(db_path, request_body(&req))?,
        ("POST", "/api/import-csv") => api_import_csv(db_path, request_body(&req))?,
        ("POST", "/api/delete-run") => api_delete_run(db_path, request_body(&req))?,
        _ => json_error(404, "not found"),
    };

    stream.write_all(&response)?;
    Ok(())
}

fn api_summary(db_path: &Path, params: &HashMap<String, String>) -> Result<Vec<u8>> {
    let db = Db::open_readonly(db_path)?;
    let run_id = run_id(&db, params)?;
    let sort = SortSpec::from_key(params.get("sort").map(String::as_str).unwrap_or("device"));
    let limit = int_param(params, "limit", 100).clamp(1, 1000);
    let q = params
        .get("q")
        .map(String::as_str)
        .filter(|v| !v.is_empty());
    let payload = SummaryPayload {
        run_id,
        totals: db.totals(run_id)?,
        rows: db.summary(run_id, q, sort, limit)?,
    };
    json_response(&payload)
}

fn api_calls(db_path: &Path, params: &HashMap<String, String>) -> Result<Vec<u8>> {
    let db = Db::open_readonly(db_path)?;
    let run_id = run_id(&db, params)?;
    let limit = int_param(params, "limit", 200).clamp(1, 1000);
    let offset = int_param(params, "offset", 0).max(0);
    let call_order = params.get("call_order").and_then(|v| v.parse::<i64>().ok());
    let payload = CallsPayload {
        run_id,
        rows: db.calls(
            run_id,
            params
                .get("q")
                .map(String::as_str)
                .filter(|v| !v.is_empty()),
            params
                .get("op")
                .map(String::as_str)
                .filter(|v| !v.is_empty()),
            call_order,
            limit,
            offset,
        )?,
    };
    json_response(&payload)
}

fn api_call_context(db_path: &Path, params: &HashMap<String, String>) -> Result<Vec<u8>> {
    let db = Db::open_readonly(db_path)?;
    let run_id = run_id(&db, params)?;
    let call_order = int_param(params, "call_order", 0);
    if call_order <= 0 {
        return Ok(json_error(400, "call_order is required"));
    }
    let radius = int_param(params, "radius", 5).clamp(1, 50);
    let payload = ContextPayload {
        run_id,
        call_order,
        radius,
        rows: db.call_context(run_id, call_order, radius)?,
    };
    json_response(&payload)
}

fn api_delete_run(db_path: &Path, body: &str) -> Result<Vec<u8>> {
    let value: serde_json::Value = serde_json::from_str(body).unwrap_or_else(|_| json!({}));
    let run_id = value
        .get("run_id")
        .and_then(|value| value.as_i64())
        .unwrap_or(0);
    if run_id <= 0 {
        return Ok(json_error(400, "run_id is required"));
    }
    Db::delete_run(db_path, run_id)?;
    json_response(&json!({ "ok": true, "deleted_run_id": run_id }))
}

fn api_import_trace(db_path: &Path, body: &str) -> Result<Vec<u8>> {
    let value: serde_json::Value = serde_json::from_str(body).unwrap_or_else(|_| json!({}));
    let trace = value
        .get("trace")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("trace path is required"))?;
    let mut options = ImportTraceOptions::new(db_path.to_path_buf(), PathBuf::from(trace));
    options.label = non_empty_string(&value, "label");
    options.categories =
        non_empty_string(&value, "categories").unwrap_or_else(|| "kernel".to_string());
    options.all_categories = value
        .get("all_categories")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    options.include_name = non_empty_string(&value, "include_name");
    options.exclude_name = non_empty_string(&value, "exclude_name");
    options.min_device_time_us = value
        .get("min_device_time_us")
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0);
    options.progress_interval = 0;
    let result = import_trace(options)?;
    json_response(&json!({ "ok": true, "result": result }))
}

fn api_import_csv(db_path: &Path, body: &str) -> Result<Vec<u8>> {
    let value: serde_json::Value = serde_json::from_str(body).unwrap_or_else(|_| json!({}));
    let csv = value
        .get("csv")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("CSV path is required"))?;
    let mut options = ImportCsvOptions::new(db_path.to_path_buf(), PathBuf::from(csv));
    options.label = non_empty_string(&value, "label");
    options.progress_interval = 0;
    let result = import_csv(options)?;
    json_response(&json!({ "ok": true, "result": result }))
}

fn download_summary(db_path: &Path, params: &HashMap<String, String>) -> Result<Vec<u8>> {
    let db = Db::open_readonly(db_path)?;
    let run_id = run_id(&db, params)?;
    let q = params
        .get("q")
        .map(String::as_str)
        .filter(|v| !v.is_empty());
    let rows = db.summary(run_id, q, SortSpec::from_key("first"), 10_000_000)?;
    csv_response("summary.csv", rows)
}

fn download_calls(db_path: &Path, params: &HashMap<String, String>) -> Result<Vec<u8>> {
    let db = Db::open_readonly(db_path)?;
    let run_id = run_id(&db, params)?;
    let limit = int_param(params, "limit", 1_000_000).clamp(1, 10_000_000);
    let rows = db.calls(run_id, None, None, None, limit, 0)?;
    csv_response("calls.csv", rows)
}

fn run_id(db: &Db, params: &HashMap<String, String>) -> Result<i64> {
    if let Some(raw) = params.get("run_id").filter(|v| !v.is_empty()) {
        return Ok(raw.parse()?);
    }
    db.latest_run_id()
}

fn request_body(request: &str) -> &str {
    request
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or("")
}

fn int_param(params: &HashMap<String, String>, key: &str, default: i64) -> i64 {
    params
        .get(key)
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(default)
}

fn non_empty_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn parse_uri(uri: &str) -> (String, HashMap<String, String>) {
    let (path, query) = uri.split_once('?').unwrap_or((uri, ""));
    let mut params = HashMap::new();
    for part in query.split('&').filter(|v| !v.is_empty()) {
        let (k, v) = part.split_once('=').unwrap_or((part, ""));
        let key = urlencoding::decode(k).unwrap_or_default().into_owned();
        let value = urlencoding::decode(v).unwrap_or_default().into_owned();
        params.insert(key, value);
    }
    (path.to_string(), params)
}

fn json_response<T: Serialize>(payload: &T) -> Result<Vec<u8>> {
    let body = serde_json::to_vec(payload)?;
    Ok(response(200, "application/json; charset=utf-8", body, None))
}

fn json_error(status: u16, message: &str) -> Vec<u8> {
    let body = serde_json::to_vec(&json!({ "error": message })).unwrap_or_default();
    response(status, "application/json; charset=utf-8", body, None)
}

fn html_response(body: &str) -> Vec<u8> {
    response(
        200,
        "text/html; charset=utf-8",
        body.as_bytes().to_vec(),
        None,
    )
}

fn csv_response<T: Serialize>(filename: &str, rows: Vec<T>) -> Result<Vec<u8>> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    for row in rows {
        writer.serialize(row)?;
    }
    let body = writer.into_inner().context("finalize csv")?;
    Ok(response(
        200,
        "text/csv; charset=utf-8",
        body,
        Some(format!("attachment; filename=\"{filename}\"")),
    ))
}

fn response(
    status: u16,
    content_type: &str,
    body: Vec<u8>,
    disposition: Option<String>,
) -> Vec<u8> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "OK",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(disposition) = disposition {
        head.push_str(&format!("Content-Disposition: {disposition}\r\n"));
    }
    head.push_str("\r\n");
    let mut out = head.into_bytes();
    out.extend(body);
    out
}

const INDEX_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>GPU Trace Viewer</title>
<style>
:root { color-scheme: light; --bg:#f6f7f9; --panel:#fff; --line:#d9dee7; --text:#171b22; --muted:#657084; --accent:#0f766e; --danger:#b42318; --soft:#e6f3f1; }
* { box-sizing: border-box; }
body { margin:0; font:14px/1.45 system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; background:var(--bg); color:var(--text); }
header { height:54px; display:flex; align-items:center; justify-content:space-between; padding:0 20px; border-bottom:1px solid var(--line); background:var(--panel); }
h1 { margin:0; font-size:17px; }
main { padding:16px 20px 24px; }
.toolbar { display:grid; grid-template-columns:minmax(180px,1.2fr) minmax(220px,2fr) 150px 110px 105px 120px auto auto; gap:10px; align-items:end; margin-bottom:12px; }
.import-panel { display:grid; grid-template-columns:130px minmax(240px,2fr) minmax(160px,1fr) 130px 130px auto auto; gap:10px; align-items:end; margin-bottom:12px; }
label { display:grid; gap:5px; color:var(--muted); font-size:12px; }
select,input,button { height:34px; border:1px solid var(--line); border-radius:6px; background:#fff; color:var(--text); padding:0 10px; font:inherit; }
button { cursor:pointer; background:var(--accent); color:#fff; border-color:var(--accent); font-weight:650; }
button.secondary { background:#fff; color:var(--text); border-color:var(--line); }
button.danger { background:var(--danger); border-color:var(--danger); }
.metrics { display:grid; grid-template-columns:repeat(5,minmax(130px,1fr)); gap:10px; margin-bottom:14px; }
.metric { background:var(--panel); border:1px solid var(--line); border-radius:8px; padding:10px 12px; }
.metric span { display:block; color:var(--muted); font-size:12px; }
.metric strong { display:block; margin-top:3px; font-size:17px; }
.split { display:grid; grid-template-columns:minmax(0,1.1fr) minmax(0,.9fr); gap:14px; align-items:start; }
.section-head { display:flex; align-items:center; justify-content:space-between; margin:4px 0 8px; }
h2 { margin:0; font-size:14px; }
.table-wrap { overflow:auto; border:1px solid var(--line); border-radius:8px; background:var(--panel); max-height:calc(100vh - 290px); }
table { width:100%; border-collapse:collapse; table-layout:fixed; }
th,td { padding:7px 9px; border-bottom:1px solid var(--line); text-align:right; white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
th { position:sticky; top:0; z-index:1; background:#f8fafc; color:var(--muted); font-size:12px; font-weight:650; }
td.name, th.name { text-align:left; }
tr[data-op], tr[data-call-order] { cursor:pointer; }
tr[data-op]:hover, tr.selected, tr[data-call-order]:hover { background:var(--soft); }
.context { position:fixed; z-index:20; width:min(760px,calc(100vw - 24px)); max-height:360px; overflow:auto; padding:10px; border:1px solid var(--line); border-radius:8px; background:var(--panel); box-shadow:0 12px 32px rgba(15,23,42,.18); }
.context[hidden] { display:none; }
.context .title { display:flex; justify-content:space-between; gap:12px; margin-bottom:7px; color:var(--muted); font-size:12px; }
.context th,.context td { padding:5px 7px; font-size:12px; }
.context tr.center { background:#fff7ed; font-weight:650; }
.status { color:var(--muted); font-size:12px; }
a { color:var(--accent); text-decoration:none; }
@media (max-width:1000px) { .toolbar,.import-panel { grid-template-columns:1fr 1fr; } .metrics { grid-template-columns:1fr 1fr; } .split { grid-template-columns:1fr; } }
</style>
</head>
<body>
<header><h1>GPU Trace Viewer</h1><div class="status" id="status"></div></header>
<main>
  <div class="toolbar">
    <label>Run<select id="run"></select></label>
    <label>Filter<input id="filter" placeholder="kernel name"></label>
    <label>Sort<select id="sort"><option value="device">Device total</option><option value="free">Free total</option><option value="total">Combined total</option><option value="count">Call count</option><option value="avg_device">Device avg</option><option value="avg_free">Free avg</option><option value="occupancy">Occupancy avg</option><option value="first">First call</option></select></label>
    <label>Limit<input id="limit" type="number" min="1" max="1000" value="100"></label>
    <label>Calls<input id="callLimit" type="number" min="1" max="1000" value="200"></label>
    <label>Call order<input id="callOrder" type="number" min="1" placeholder="exact"></label>
    <button id="refresh">Refresh</button>
    <button class="secondary" id="clear">Clear</button>
  </div>
  <div class="import-panel">
    <label>Import<select id="importType"><option value="trace">Trace JSON/GZ</option><option value="csv">Calls CSV</option></select></label>
    <label>Source path<input id="importPath" placeholder="/path/to/trace.json.gz or calls.csv"></label>
    <label>Label<input id="importLabel" placeholder="optional"></label>
    <label>Categories<input id="importCategories" value="kernel"></label>
    <label>Min device us<input id="importMinDevice" type="number" min="0" value="0"></label>
    <button id="importRun">Import</button>
    <button class="danger" id="deleteRun">Delete Run</button>
  </div>
  <div class="metrics" id="metrics"></div>
  <div class="split">
    <section><div class="section-head"><h2>Summary</h2><a id="summaryCsv" href="#">CSV</a></div><div class="table-wrap"><table id="summary"><thead></thead><tbody></tbody></table></div></section>
    <section><div class="section-head"><h2>Calls</h2><a id="callsCsv" href="#">CSV</a></div><div class="table-wrap"><table id="calls"><thead></thead><tbody></tbody></table></div></section>
  </div>
</main>
<div id="context" class="context" hidden></div>
<script>
const $ = id => document.getElementById(id);
let selectedOp = "";
let selectedRow = null;
let contextTimer = 0;
let contextRequest = 0;
const nf = new Intl.NumberFormat(undefined, { maximumFractionDigits: 3 });
function num(v) { return nf.format(Number(v || 0)); }
function ms(us) { return nf.format(Number(us || 0) / 1000); }
function maybe(v) { return v === null || v === undefined || v === "" ? "" : nf.format(Number(v)); }
function kib(v) { return v === null || v === undefined || v === "" ? "" : nf.format(Number(v) / 1024); }
function esc(s) { return String(s ?? "").replace(/[&<>"]/g, c => ({"&":"&amp;","<":"&lt;",">":"&gt;","\"":"&quot;"}[c])); }
function status(s) { $("status").textContent = s; }
async function getJSON(url) { const r = await fetch(url); const j = await r.json(); if (!r.ok || j.error) throw new Error(j.error || r.statusText); return j; }
async function postJSON(url, payload) { const r = await fetch(url, { method:"POST", headers:{ "content-type":"application/json" }, body:JSON.stringify(payload) }); const j = await r.json(); if (!r.ok || j.error) throw new Error(j.error || r.statusText); return j; }
function runId() { return $("run").value; }
function linkParams() { return `run_id=${encodeURIComponent(runId())}`; }
function positionContext(e) { const box = $("context"); const m = 12; let left = e.clientX + m; let top = e.clientY + m; box.style.left = `${left}px`; box.style.top = `${top}px`; requestAnimationFrame(() => { const r = box.getBoundingClientRect(); box.style.left = `${Math.max(m, Math.min(left, window.innerWidth - r.width - m))}px`; box.style.top = `${Math.max(m, Math.min(top, window.innerHeight - r.height - m))}px`; }); }
function hideContext() { clearTimeout(contextTimer); contextRequest += 1; $("context").hidden = true; }
function renderContext(data) { const rows = data.rows || []; $("context").innerHTML = `<div class="title"><strong>Call ${data.call_order} +/- ${data.radius}</strong><span>${rows.length} calls</span></div><table><thead><tr><th>Order</th><th>Device us</th><th>Free us</th><th>Occ %</th><th class="name">Kernel</th></tr></thead><tbody>${rows.map(r => `<tr class="${Number(r.call_order) === Number(data.call_order) ? "center" : ""}"><td>${r.call_order}</td><td>${num(r.device_time_us)}</td><td>${num(r.free_time_us)}</td><td>${maybe(r.occupancy_pct)}</td><td class="name" title="${esc(r.op_name)}">${esc(r.op_name)}</td></tr>`).join("")}</tbody></table>`; }
function showContext(row, e) { const callOrder = row.dataset.callOrder; if (!callOrder || !runId()) return; const requestId = ++contextRequest; clearTimeout(contextTimer); positionContext(e); contextTimer = setTimeout(async () => { const params = new URLSearchParams({ run_id: runId(), call_order: callOrder, radius: "5" }); const data = await getJSON(`/api/call-context?${params}`); if (requestId !== contextRequest) return; renderContext(data); $("context").hidden = false; positionContext(e); }, 120); }
async function loadRuns(selectedId = "") { const data = await getJSON("/api/runs"); $("run").innerHTML = data.runs.map(r => `<option value="${r.id}">#${r.id} ${esc(r.label || r.source_path)}</option>`).join(""); if (selectedId) $("run").value = String(selectedId); if (!data.runs.length) { renderMetrics({}); renderSummary([]); renderCalls([]); status("No runs imported"); } }
function renderMetrics(t) { $("metrics").innerHTML = [["Unique ops", num(t.unique_ops)], ["Calls", num(t.call_count)], ["Device ms", ms(t.total_device_time_us)], ["Free ms", ms(t.total_free_time_us)], ["Avg occupancy %", maybe(t.avg_occupancy_pct)]].map(([k,v]) => `<div class="metric"><span>${k}</span><strong>${v}</strong></div>`).join(""); }
function renderSummary(rows) { $("summary").tHead.innerHTML = `<tr><th>First</th><th>Calls</th><th>Device ms</th><th>Avg device us</th><th>Occ %</th><th>Free ms</th><th>Total ms</th><th class="name">Kernel</th></tr>`; $("summary").tBodies[0].innerHTML = rows.map(r => `<tr data-op="${encodeURIComponent(r.op_name)}"><td>${r.first_call_order}</td><td>${num(r.call_count)}</td><td>${ms(r.total_device_time_us)}</td><td>${maybe(r.avg_device_time_us)}</td><td>${maybe(r.avg_occupancy_pct)}</td><td>${ms(r.total_free_time_us)}</td><td>${ms(r.total_time_us)}</td><td class="name" title="${esc(r.op_name)}">${esc(r.op_name)}</td></tr>`).join(""); [...$("summary").querySelectorAll("tr[data-op]")].forEach(tr => tr.addEventListener("click", () => { if (selectedRow) selectedRow.classList.remove("selected"); selectedRow = tr; tr.classList.add("selected"); selectedOp = decodeURIComponent(tr.dataset.op); $("callOrder").value = ""; loadCalls(); })); }
function renderCalls(rows) { hideContext(); $("calls").tHead.innerHTML = `<tr><th>Order</th><th>Dev</th><th>Stream</th><th>Device us</th><th>Occ %</th><th>Free us</th><th>Total us</th><th>Blocks/SM</th><th>Warps/SM</th><th>Shmem KiB</th><th>Grid</th><th>Block</th><th class="name">Kernel</th></tr>`; $("calls").tBodies[0].innerHTML = rows.map(r => `<tr data-call-order="${r.call_order}"><td>${r.call_order}</td><td>${esc(r.device)}</td><td>${esc(r.stream)}</td><td>${num(r.device_time_us)}</td><td>${maybe(r.occupancy_pct)}</td><td>${num(r.free_time_us)}</td><td>${num(r.total_time_us)}</td><td>${maybe(r.blocks_per_sm)}</td><td>${maybe(r.warps_per_sm)}</td><td>${kib(r.shared_memory)}</td><td>${esc(r.grid)}</td><td>${esc(r.block)}</td><td class="name" title="${esc(r.op_name)}">${esc(r.op_name)}</td></tr>`).join(""); [...$("calls").querySelectorAll("tr[data-call-order]")].forEach(tr => { tr.addEventListener("mouseenter", e => showContext(tr, e)); tr.addEventListener("mousemove", positionContext); tr.addEventListener("mouseleave", hideContext); }); }
async function loadSummary() { if (!runId()) return; status("Loading summary"); selectedOp = ""; selectedRow = null; const params = new URLSearchParams({ run_id: runId(), q: $("filter").value, sort: $("sort").value, limit: $("limit").value }); const data = await getJSON(`/api/summary?${params}`); renderMetrics(data.totals || {}); renderSummary(data.rows || []); $("summaryCsv").href = `/download/summary.csv?${linkParams()}`; await loadCalls(); status(`Run #${data.run_id}`); }
async function loadCalls() { if (!runId()) return; const params = new URLSearchParams({ run_id: runId(), limit: $("callLimit").value }); const order = $("callOrder").value.trim(); if (order) params.set("call_order", order); else if (selectedOp) params.set("op", selectedOp); else if ($("filter").value) params.set("q", $("filter").value); const data = await getJSON(`/api/calls?${params}`); renderCalls(data.rows || []); $("callsCsv").href = `/download/calls.csv?${linkParams()}`; }
async function importRun() { const path = $("importPath").value.trim(); if (!path) { status("Source path is required"); return; } const type = $("importType").value; const payload = { label:$("importLabel").value.trim() }; status("Importing. Large files can take a while"); const data = type === "csv" ? await postJSON("/api/import-csv", { ...payload, csv:path }) : await postJSON("/api/import-trace", { ...payload, trace:path, categories:$("importCategories").value.trim() || "kernel", min_device_time_us:Number($("importMinDevice").value || 0) }); const runId = data.result.run_id; status(`Imported run #${runId}: ${num(data.result.gpu_calls)} calls`); await loadRuns(runId); await loadSummary(); }
async function deleteRun() { if (!runId()) return; const label = $("run").selectedOptions[0]?.textContent || `run #${runId()}`; if (!confirm(`Delete ${label} from SQLite? Original trace files are not deleted.`)) return; const data = await postJSON("/api/delete-run", { run_id:Number(runId()) }); status(`Deleted run #${data.deleted_run_id}`); await loadRuns(); await loadSummary(); }
$("refresh").addEventListener("click", loadSummary);
$("clear").addEventListener("click", () => { $("filter").value = ""; $("callOrder").value = ""; selectedOp = ""; loadSummary(); });
$("importRun").addEventListener("click", () => importRun().catch(e => status(e.message)));
$("deleteRun").addEventListener("click", () => deleteRun().catch(e => status(e.message)));
$("run").addEventListener("change", loadSummary); $("sort").addEventListener("change", loadSummary); $("limit").addEventListener("change", loadSummary); $("callLimit").addEventListener("change", loadCalls); $("callOrder").addEventListener("change", loadCalls); $("callOrder").addEventListener("keydown", e => { if (e.key === "Enter") loadCalls(); }); $("filter").addEventListener("keydown", e => { if (e.key === "Enter") loadSummary(); });
loadRuns().then(loadSummary).catch(e => status(e.message));
</script>
</body>
</html>"##;
