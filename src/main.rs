mod db;
mod importer;
mod tui;
mod web;

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::db::{Db, SortSpec};
use crate::importer::{ImportCsvOptions, ImportTraceOptions, import_csv, import_trace};

#[derive(Debug, Parser)]
#[command(name = "gpu-trace-viewer")]
#[command(about = "Terminal and web viewer for gpu_trace_stats.sqlite", version)]
struct Cli {
    #[arg(long, global = true, value_name = "PATH")]
    db: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
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
    ImportTrace {
        trace: PathBuf,
        #[arg(long)]
        label: Option<String>,
        #[arg(long, default_value = "kernel")]
        categories: String,
        #[arg(long)]
        all_categories: bool,
        #[arg(long)]
        include_name: Option<String>,
        #[arg(long)]
        exclude_name: Option<String>,
        #[arg(long, alias = "min-duration-us", default_value_t = 0.0)]
        min_device_time_us: f64,
        #[arg(long, default_value_t = 1000)]
        batch_size: usize,
        #[arg(long, default_value_t = 100_000)]
        progress_interval: i64,
    },
    ImportCsv {
        csv: PathBuf,
        #[arg(long)]
        label: Option<String>,
        #[arg(long, default_value_t = 1000)]
        batch_size: usize,
        #[arg(long, default_value_t = 100_000)]
        progress_interval: i64,
    },
    DeleteRun {
        run_id: i64,
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
    let command = cli.command.unwrap_or(Command::Tui {
        summary_limit: 200,
        calls_limit: 200,
    });

    match command {
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
            println!(
                "order\tdevice\tstream\tdevice_us\tfree_us\ttotal_us\tocc_pct\tblocks_per_sm\twarps_per_sm\tshared_memory\tgrid\tblock\top"
            );
            for row in rows {
                println!(
                    "{}\t{}\t{}\t{:.3}\t{:.3}\t{:.3}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    row.call_order,
                    row.device.unwrap_or_default(),
                    row.stream.unwrap_or_default(),
                    row.device_time_us,
                    row.free_time_us,
                    row.total_time_us,
                    row.occupancy_pct
                        .map(|v| format!("{v:.3}"))
                        .unwrap_or_default(),
                    row.blocks_per_sm
                        .map(|v| format!("{v:.3}"))
                        .unwrap_or_default(),
                    row.warps_per_sm
                        .map(|v| format!("{v:.3}"))
                        .unwrap_or_default(),
                    row.shared_memory
                        .map(|v| format!("{v:.0}"))
                        .unwrap_or_default(),
                    row.grid.unwrap_or_default(),
                    row.block.unwrap_or_default(),
                    row.op_name
                );
            }
            Ok(())
        }
        Command::ImportTrace {
            trace,
            label,
            categories,
            all_categories,
            include_name,
            exclude_name,
            min_device_time_us,
            batch_size,
            progress_interval,
        } => {
            let mut options = ImportTraceOptions::new(db_path, trace);
            options.label = label;
            options.categories = categories;
            options.all_categories = all_categories;
            options.include_name = include_name;
            options.exclude_name = exclude_name;
            options.min_device_time_us = min_device_time_us;
            options.batch_size = batch_size;
            options.progress_interval = progress_interval;
            let result = import_trace(options)?;
            println!("db: {}", result.db_path.display());
            println!("run_id: {}", result.run_id);
            println!("gpu_calls: {}", result.gpu_calls);
            println!("unique_ops: {}", result.unique_ops);
            Ok(())
        }
        Command::ImportCsv {
            csv,
            label,
            batch_size,
            progress_interval,
        } => {
            let mut options = ImportCsvOptions::new(db_path, csv);
            options.label = label;
            options.batch_size = batch_size;
            options.progress_interval = progress_interval;
            let result = import_csv(options)?;
            println!("db: {}", result.db_path.display());
            println!("run_id: {}", result.run_id);
            println!("gpu_calls: {}", result.gpu_calls);
            println!("unique_ops: {}", result.unique_ops);
            Ok(())
        }
        Command::DeleteRun { run_id } => {
            Db::delete_run(&db_path, run_id)?;
            println!("deleted run_id: {run_id}");
            Ok(())
        }
    }
}
