use std::env;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use logflayer::api::{self, ApiState};
use logflayer::backfill::{self, BackfillOptions};
use logflayer::config::AppConfig;
use logflayer::error::AppError;
use logflayer::logging::init_logging;
use logflayer::metrics;
use logflayer::preprocessing::PREPROCESSING_VERSION;
use logflayer::repository::MongoRepository;
use logflayer::service::Application;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> Result<(), AppError> {
    dotenvy::dotenv().ok();

    let args: Vec<String> = env::args().collect();

    match args.get(1).map(String::as_str) {
        Some("backfill") => run_backfill(&args[2..]).await,
        Some("smoketest") => run_smoketest(&args[2..]).await,
        _ => run_service().await,
    }
}

// ─── Normal service mode ───────────────────────────────────────────────────────

async fn run_service() -> Result<(), AppError> {
    let config = AppConfig::from_env()?;
    let _log_guard = init_logging(&config.logging)?;

    // ── Apply MongoDB admin overrides + canary-confirmation check ─────────────
    //
    // After every `PUT /admin/settings` the document is stored with
    // `_confirmed = false`.  If this process started with such a document we
    // spawn a 15-second rollback timer.  The timer is cancelled (by the
    // database `_confirmed` flag being flipped) when the frontend sends
    // `POST /api/v1/admin/confirm`.

    let pending_confirmation = Arc::new(AtomicBool::new(false));

    let config = match MongoRepository::connect(&config.mongo).await {
        Ok(repo) => {
            // Check pending-confirmation state BEFORE loading settings so we
            // can capture the rollback target.
            let pending_meta = repo.load_config_pending_meta().await.unwrap_or(None);

            let config = match repo.load_admin_settings().await {
                Ok(Some(overrides)) => {
                    info!("applying admin settings overrides from MongoDB");
                    config.apply_admin_settings(overrides)
                }
                Ok(None) => config,
                Err(e) => {
                    warn!(error = %e, "could not load admin settings — using env defaults");
                    config
                }
            };

            // If the stored config is unconfirmed, start the rollback timer.
            if let Some((confirmed, _previous)) = pending_meta {
                if !confirmed {
                    info!("started with unconfirmed config — rollback timer armed (15 s)");
                    pending_confirmation.store(true, Ordering::Relaxed);

                    let repo_clone   = repo.clone();
                    let pc_clone     = pending_confirmation.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_secs(15)).await;

                        // Re-read the flag from the DB (frontend may have confirmed).
                        let still_pending = match repo_clone.load_config_pending_meta().await {
                            Ok(Some((confirmed, _))) => !confirmed,
                            _ => false,
                        };

                        if still_pending {
                            warn!(
                                "config not confirmed within 15 s — rolling back to previous \
                                 settings and restarting"
                            );
                            match repo_clone
                                .rollback_admin_settings(
                                    "frontend did not confirm within 15 s",
                                )
                                .await
                            {
                                Ok(Some(_)) => info!("rollback complete — restarting"),
                                Ok(None)    => warn!("rollback: no previous settings found"),
                                Err(e)      => error!(error = %e, "rollback write failed"),
                            }
                            std::process::exit(0);
                        } else {
                            info!("config confirmed before deadline — rollback timer disarmed");
                            pc_clone.store(false, Ordering::Relaxed);
                        }
                    });
                }
            }

            config
        }
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
        match MongoRepository::connect(&config.mongo).await {
            Ok(repo) => {
                if let Err(e) = repo.ping().await {
                    warn!(error = %e, "MongoDB ping failed — API will start in degraded mode");
                }
                let api_state = ApiState {
                    repo,
                    config: config.clone(),
                    sample_trigger: app.trigger.clone(),
                    pending_confirmation,
                };
                let port = config.service.api_port;
                tokio::spawn(async move {
                    api::start(api_state, port).await;
                });
            }
            Err(e) => {
                warn!(error = %e, "could not connect to MongoDB — API will not start");
            }
        }
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

// ─── Smoketest subcommand ─────────────────────────────────────────────────────

/// `logflayer smoketest <fixture-path> [--target-id ID]`
///
/// End-to-end smoke test: reads a log fixture from disk, runs it through the
/// full preprocessing + async-output pipeline (graph writer, vector writer,
/// embedding worker — whatever is enabled), and prints a human-readable
/// summary so the operator can verify wiring without setting up an
/// SSH-reachable target.
///
/// Honours all the same env vars as the live service.  The most useful ones
/// for a meaningful smoketest are:
///
///   ENTITY_EXTRACTION_ENABLED=true   # default true
///   GRAPH_WRITER_ENABLED=true        # default false
///   VECTOR_WRITER_ENABLED=true       # default false
///   EMBEDDING_ENABLED=true           # default false  (needs API key)
async fn run_smoketest(args: &[String]) -> Result<(), AppError> {
    let fixture_path = match args.first() {
        Some(p) => p.clone(),
        None => {
            eprintln!("usage: logflayer smoketest <fixture-path> [--target-id ID]");
            std::process::exit(2);
        }
    };
    let target_id = string_arg("--target-id", args).unwrap_or_else(|| "smoketest".to_string());

    let config = AppConfig::from_env()?;
    let _log_guard = init_logging(&config.logging)?;

    // Apply admin overrides exactly as the live service does, so the smoketest
    // sees the same wiring the operator actually deployed.
    let config = match MongoRepository::connect(&config.mongo).await {
        Ok(repo) => match repo.load_admin_settings().await {
            Ok(Some(overrides)) => {
                info!("applying admin settings overrides from MongoDB");
                config.apply_admin_settings(overrides)
            }
            _ => config,
        },
        Err(_) => config,
    };

    let content = std::fs::read_to_string(&fixture_path)?;

    let app = Application::build(config).await?;
    let source_label = format!("smoketest:{}", fixture_path);

    println!("┌─ Smoke test ──────────────────────────────────────────────────");
    println!("│ fixture     : {}", fixture_path);
    println!("│ target_id   : {}", target_id);
    println!("│ content_len : {} bytes / {} lines", content.len(), content.lines().count());

    let report = app.smoketest_sample(content, target_id, source_label).await?;

    println!("├─ Wiring snapshot ─────────────────────────────────────────────");
    println!("│ entity_extraction_enabled = {}", report.entity_extraction_enabled);
    println!("│ min_entities_for_persist  = {}", report.min_entities_for_persist);
    println!("│ graph_writer_enabled      = {}", report.graph_writer_enabled);
    println!("│ vector_writer_enabled     = {}", report.vector_writer_enabled);
    println!("│ embedding_enabled         = {}", report.embedding_enabled);
    println!("├─ Pipeline result ─────────────────────────────────────────────");
    println!("│ sample_hash       = {}", report.sample_hash);
    println!("│ sample_was_new    = {}", report.sample_was_new);
    println!("│ entities          = {}", report.entity_count);
    println!("│ relations         = {}", report.relation_count);
    println!("├─ Auxiliary collections (filtered by sample_hash) ─────────────");
    println!("│ entity_edges      = {}", report.edges_collection_total);
    println!("│ prov_relations    = {}", report.prov_collection_total);
    println!("│ otel_spans        = {}", report.spans_collection_total);
    println!("│ embeddings (c+b)  = {}", report.embeddings_collection_total);
    println!("└───────────────────────────────────────────────────────────────");

    // Diagnose the most common "empty result" reasons so the operator knows
    // why the report is what it is, without re-reading the wiring snapshot.
    if !report.entity_extraction_enabled {
        println!("note: ENTITY_EXTRACTION_ENABLED=false — Stages 6–10 were skipped.");
    } else if report.entity_count == 0 {
        println!("note: 0 entities extracted — fixture may not contain agentic patterns.");
    } else if (report.entity_count as usize) < report.min_entities_for_persist {
        println!(
            "note: entities ({}) < min_entities_for_persist ({}) — graph + vector writes were gated off.",
            report.entity_count, report.min_entities_for_persist
        );
    } else {
        if !report.graph_writer_enabled && report.edges_collection_total == 0 {
            println!("note: GRAPH_WRITER_ENABLED=false — set it to populate entity_edges/prov_relations/otel_spans.");
        }
        if !report.vector_writer_enabled && report.embeddings_collection_total == 0 {
            println!("note: VECTOR_WRITER_ENABLED=false — set it to populate {{content,behavioral}}_embeddings.");
        }
    }

    Ok(())
}

fn string_arg(flag: &str, args: &[String]) -> Option<String> {
    args.windows(2).find_map(|w| {
        if w[0] == flag {
            Some(w[1].clone())
        } else {
            None
        }
    })
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
