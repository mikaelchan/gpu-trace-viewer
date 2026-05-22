mod db;
mod tui;
mod web;

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::db::{Db, SortSpec};

#[derive(Debug, Parser)]
#[command(name = "gpu-trace-viewer")]
#[command(about = "Terminal and web viewer for gpu_trace_stats.sqlite", version)]
struct Cli {
    #[arg(long, global = true, value_name = "PATH")]
    db: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Tui {
        #[arg(long, default_value_t = 200)]
        summary_limit: i64,
        #[arg(long, default_value_t = 200)]
        calls_limit: i64,
    },
    Serve {
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 8766)]
        port: u16,
    },
    Runs,
    Top {
        #[arg(long, default_value = "device")]
        by: String,
        #[arg(long)]
        run_id: Option<i64>,
        #[arg(long)]
        q: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: i64,
    },
    Calls {
        #[arg(long)]
        run_id: Option<i64>,
        #[arg(long)]
        q: Option<String>,
        #[arg(long)]
        op: Option<String>,
        #[arg(long)]
        call_order: Option<i64>,
        #[arg(long, default_value_t = 50)]
        limit: i64,
        #[arg(long, default_value_t = 0)]
        offset: i64,
    },
}

fn default_db_path() -> PathBuf {
    for candidate in ["gpu_trace_stats.sqlite", "../gpu_trace_stats.sqlite"] {
        let path = Path::new(candidate);
        if path.exists() {
            return path.to_path_buf();
        }
    }
    PathBuf::from("gpu_trace_stats.sqlite")
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let db_path = cli.db.unwrap_or_else(default_db_path);

    match cli.command {
        Command::Tui {
            summary_limit,
            calls_limit,
        } => tui::run(db_path, summary_limit, calls_limit),
        Command::Serve { host, port } => web::serve(db_path, &host, port),
        Command::Runs => {
            let db = Db::open_readonly(&db_path)?;
            println!("id\tlabel\ttype\tcalls\tops\timported_at\tsource");
            for run in db.runs()? {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    run.id,
                    run.label.unwrap_or_default(),
                    run.source_type,
                    run.total_calls,
                    run.unique_ops,
                    run.imported_at,
                    run.source_path
                );
            }
            Ok(())
        }
        Command::Top {
            by,
            run_id,
            q,
            limit,
        } => {
            let db = Db::open_readonly(&db_path)?;
            let run_id = run_id.unwrap_or(db.latest_run_id()?);
            let sort = SortSpec::from_key(&by);
            let rows = db.summary(run_id, q.as_deref(), sort, limit)?;
            println!("run_id: {run_id}");
            println!("first\tcalls\tdevice_ms\tfree_ms\ttotal_ms\tavg_device_us\tocc_pct\top");
            for row in rows {
                println!(
                    "{}\t{}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{}\t{}",
                    row.first_call_order,
                    row.call_count,
                    row.total_device_time_us / 1000.0,
                    row.total_free_time_us / 1000.0,
                    row.total_time_us / 1000.0,
                    row.avg_device_time_us,
                    row.avg_occupancy_pct
                        .map(|v| format!("{v:.3}"))
                        .unwrap_or_default(),
                    row.op_name
                );
            }
            Ok(())
        }
        Command::Calls {
            run_id,
            q,
            op,
            call_order,
            limit,
            offset,
        } => {
            let db = Db::open_readonly(&db_path)?;
            let run_id = run_id.unwrap_or(db.latest_run_id()?);
            let rows = db.calls(
                run_id,
                q.as_deref(),
                op.as_deref(),
                call_order,
                limit,
                offset,
            )?;
            println!("run_id: {run_id}");
            println!("order\tidx\tdevice\tstream\tdevice_us\tfree_us\ttotal_us\tocc_pct\top");
            for row in rows {
                println!(
                    "{}\t{}\t{}\t{}\t{:.3}\t{:.3}\t{:.3}\t{}\t{}",
                    row.call_order,
                    row.op_call_index,
                    row.device.unwrap_or_default(),
                    row.stream.unwrap_or_default(),
                    row.device_time_us,
                    row.free_time_us,
                    row.total_time_us,
                    row.occupancy_pct
                        .map(|v| format!("{v:.3}"))
                        .unwrap_or_default(),
                    row.op_name
                );
            }
            Ok(())
        }
    }
}
