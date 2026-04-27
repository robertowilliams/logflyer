use std::io;

use mongodb::error::Error as MongoError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("configuration error: {0}")]
    Config(#[from] ConfigError),
    #[error("mongodb error: {0}")]
    Mongo(#[from] MongoError),
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("ssh error: {0}")]
    Ssh(String),
    #[error("validation error: {0}")]
    Validation(String),
    #[error("task join error: {0}")]
    Join(String),
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("missing required environment variable `{0}`")]
    MissingVar(String),
    #[error("invalid value for environment variable `{0}`: `{1}`")]
    InvalidVar(String, String),
}
