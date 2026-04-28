use std::env;

use logflayer::api::{self, ApiState};
use logflayer::backfill::{self, BackfillOptions};
use logflayer::config::AppConfig;
use logflayer::error::AppError;
use logflayer::logging::init_logging;
use logflayer::metrics;
use logflayer::preprocessing::PREPROCESSING_VERSION;
use logflayer::repository::MongoRepository;
use logflayer::service::Application;
use tracing::{info, warn};

#[tokio::main]
async fn main() -> Result<(), AppError> {
    dotenvy::dotenv().ok();

    let args: Vec<String> = env::args().collect();

    match args.get(1).map(String::as_str) {
        Some("backfill") => run_backfill(&args[2..]).await,
        _ => run_service().await,
    }
}

// ─── Normal service mode ───────────────────────────────────────────────────────

async fn run_service() -> Result<(), AppError> {
    let config = AppConfig::from_env()?;
    let _log_guard = init_logging(&config.logging)?;

    // Apply any admin settings persisted in MongoDB on top of the env baseline.
    let config = match MongoRepository::connect(&config.mongo).await {
        Ok(repo) => match repo.load_admin_settings().await {
            Ok(Some(overrides)) => {
                info!("applying admin settings overrides from MongoDB");
                config.apply_admin_settings(overrides)
            }
            Ok(None) => config,
            Err(e) => {
                warn!(error = %e, "could not load admin settings — using env defaults");
                config
            }
        },
        Err(e) => {
            warn!(error = %e, "could not connect to load admin settings — using env defaults");
            config
        }
    };

    if config.preprocessing.metrics_port > 0 {
        metrics::install(config.preprocessing.metrics_port);
    }

    if logflayer::config::bool_flag_pub("PREPROCESSING_REPROCESS_ON_VERSION_CHANGE", false)
        && config.preprocessing.enabled
    {
        let repository = MongoRepository::connect(&config.mongo).await?;
        repository.ping().await?;
        backfill::purge_stale_metadata(&repository, PREPROCESSING_VERSION).await?;
    }

    let app = Application::build(config.clone()).await?;

    // Share the trigger with the API so POST /api/v1/sample fires an immediate cycle.
    if config.service.api_port > 0 {
        let repo = MongoRepository::connect(&config.mongo).await?;
        repo.ping().await?;
        let api_state = ApiState {
            repo,
            config: config.clone(),
            sample_trigger: app.trigger.clone(),
        };
        let port = config.service.api_port;
        tokio::spawn(async move {
            api::start(api_state, port).await;
        });
    }

    app.run().await
}

// ─── Backfill subcommand ───────────────────────────────────────────────────────

async fn run_backfill(args: &[String]) -> Result<(), AppError> {
    let config = AppConfig::from_env()?;
    let _log_guard = init_logging(&config.logging)?;

    let mut opts = BackfillOptions {
        batch_size: positive_usize_arg("--batch-size", args).unwrap_or(100),
        dry_run: args.iter().any(|a| a == "--dry-run"),
        reprocess_stale: args.iter().any(|a| a == "--reprocess_stale"),
    };

    if opts.batch_size == 0 {
        opts.batch_size = 1;
    }

    if opts.reprocess_stale && config.preprocessing.enabled {
        let repository = MongoRepository::connect(&config.mongo).await?;
        repository.ping().await?;
        backfill::purge_stale_metadata(&repository, PREPROCESSING_VERSION).await?;
    }

    let summary = backfill::run(config, opts).await?;

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

fn positive_usize_arg(flag: &str, args: &[String]) -> Option<usize> {
    args.windows(2).find_map(|w| {
        if w[0] == flag {
            w[1].parse::<usize>().ok()
        } else {
            None
        }
    })
}
