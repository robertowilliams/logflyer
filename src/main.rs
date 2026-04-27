use std::env;

use logflayer::backfill::{self, BackfillOptions};
use logflayer::config::AppConfig;
use logflayer::error::AppError;
use logflayer::logging::init_logging;
use logflayer::metrics;
use logflayer::preprocessing::PREPROCESSING_VERSION;
use logflayer::repository::MongoRepository;
use logflayer::service::Application;

#[tokio::main]
async fn main() -> Result<(), AppError> {
    dotenvy::dotenv().ok();

    let args: Vec<String> = env::args().collect();

    // Dispatch on the first positional argument.  The default (no subcommand)
    // runs the normal sampling service loop.
    if args.get(1).map(String::as_str) == Some("backfill") {
        return run_backfill(&args[2..]).await;
    }

    run_service().await
}

// ─── Normal service mode ───────────────────────────────────────────────────────

async fn run_service() -> Result<(), AppError> {
    let config = AppConfig::from_env()?;
    let _log_guard = init_logging(&config.logging)?;

    // Start the Prometheus HTTP listener unless the operator set METRICS_PORT=0.
    if config.preprocessing.metrics_port > 0 {
        metrics::install(config.preprocessing.metrics_port);
    }

    // If the operator has opted in to version-aware reprocessing, purge any
    // metadata that was produced by an older preprocessing pipeline version
    // before the service starts accepting new samples.
    if logflayer::config::bool_flag_pub("PREPROCESSING_REPROCESS_ON_VERSION_CHANGE", false)
        && config.preprocessing.enabled
    {
        let repository = MongoRepository::connect(&config.mongo).await?;
        repository.ping().await?;

        backfill::purge_stale_metadata(&repository, PREPROCESSING_VERSION).await?;
    }

    let app = Application::build(config).await?;
    app.run().await
}

// ─── Backfill subcommand ───────────────────────────────────────────────────────

/// Parse remaining args after the `backfill` keyword and run the job.
///
/// Usage: `logflayer backfill [--batch-size N] [--dry-run] [--reprocess-stale]`
async fn run_backfill(args: &[String]) -> Result<(), AppError> {
    let config = AppConfig::from_env()?;
    let _log_guard = init_logging(&config.logging)?;

    let mut opts = BackfillOptions {
        batch_size: positive_usize_arg("--batch-size", args).unwrap_or(100),
        dry_run: args.iter().any(|a| a == "--dry-run"),
        reprocess_stale: args.iter().any(|a| a == "--reprocess-stale"),
    };

    // --batch-size 0 is nonsensical; clamp to 1.
    if opts.batch_size == 0 {
        opts.batch_size = 1;
    }

    if opts.reprocess_stale && config.preprocessing.enabled {
        // Before running the main loop, purge metadata from older pipeline
        // versions so the backfill loop will re-process those samples.
        let repository = MongoRepository::connect(&config.mongo).await?;
        repository.ping().await?;

        backfill::purge_stale_metadata(&repository, PREPROCESSING_VERSION).await?;
    }

    let summary = backfill::run(config, opts).await?;

    // Print a human-readable summary to stdout (structured JSON is in the logs).
    println!("Backfill complete:");
    println!("  attempted : {}", summary.attempted);
    println!("  written   : {}", summary.written);
    println!("  failed    : {}", summary.failed);
    println!("  agentic   : {}", summary.agentic);
    println!("  elapsed   : {:.2}s", summary.elapsed_secs);
    if summary.dry_run {
        println!("  [DRY RUN — nothing was written]");
    }

    Ok(())
}

// ─── Arg helpers ──────────────────────────────────────────────────────────────

/// Extract the `usize` value following a named flag, e.g. `--batch-size 50`.
fn positive_usize_arg(flag: &str, args: &[String]) -> Option<usize> {
    args.windows(2).find_map(|w| {
        if w[0] == flag {
            w[1].parse::<usize>().ok()
        } else {
            None
        }
    })
}
