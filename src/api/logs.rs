use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Seek, SeekFrom};

use super::SharedState;

#[derive(Deserialize)]
pub struct LogsQuery {
    #[serde(default = "default_lines")]
    lines: usize,
}

fn default_lines() -> usize {
    200
}

/// Return the last `lines` lines from the logflayer log file.
pub async fn list(
    State(s): State<SharedState>,
    Query(q): Query<LogsQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let log_dir = s.config.logging.directory.clone();
    let base = s.config.logging.file_base_name.clone();

    let log_path = log_dir.join(format!("{}.log", base));

    let lines = match read_tail(&log_path, q.lines) {
        Ok(l) => l,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("could not read log file: {e}") })),
            ))
        }
    };

    Ok(Json(json!({
        "lines": lines,
        "total": lines.len(),
        "log_file": log_path.display().to_string(),
    })))
}

fn read_tail(path: &std::path::Path, n: usize) -> std::io::Result<Vec<String>> {
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);

    // Collect all lines into a ring buffer of size n.
    let mut ring: Vec<String> = Vec::with_capacity(n + 1);
    let mut line = String::new();

    loop {
        line.clear();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            break;
        }
        if ring.len() == n {
            ring.remove(0);
        }
        ring.push(line.trim_end().to_string());
    }

    Ok(ring)
}
