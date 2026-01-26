// SPDX-License-Identifier: LGPL-2.1-only
// Copyright (C) 2025 Collabora Ltd
// Author: Denys Fedoryshchenko <denys.f@collabora.com>
//
// This library is free software; you can redistribute it and/or modify it under
// the terms of the GNU Lesser General Public License as published by the Free
// Software Foundation; version 2.1.
//
// This library is distributed in the hope that it will be useful, but WITHOUT ANY
// WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A
// PARTICULAR PURPOSE. See the GNU Lesser General Public License for more details.
//
// You should have received a copy of the GNU Lesser General Public License along
// with this library; if not, write to the Free Software Foundation, Inc.,
// 51 Franklin Street, Fifth Floor, Boston, MA 02110-1301 USA

/*
KCIDB-Rust REST submissions receiver

1)Verify user authentication
2)Create file name with suffix _temp, until it is ready to be
   processed
3)After all file received, rename the file to the final name
4)Validate if the submission is valid JSON
* Optionally validate some other things

*/

use axum::Router;
use axum::extract::{Multipart, State};
use axum::http::StatusCode;
use axum::http::header::{HeaderMap, CONTENT_LENGTH};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::body::Bytes;
use axum_server::tls_rustls::RustlsConfig;
use clap::Parser;
use jsonwebtoken::{DecodingKey, Validation, decode};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use tokio::net::TcpListener;
use tower_http::limit::RequestBodyLimitLayer;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// The port to listen on
    #[clap(short, long, default_value = "0")]
    port: u16,
    /// The host to listen on
    #[clap(short = 'b', long, default_value = "0.0.0.0")]
    host: String,
    /// The path to the directory to store the received files
    #[clap(short = 'd', long, default_value = "/app/spool")]
    directory: String,
    /// JWT secret
    #[clap(short, long, default_value = "secret")]
    jwt_secret: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct SubmissionStatus {
    id: String,
    status: String,
    message: Option<String>,
}

use std::sync::atomic::{AtomicU64, Ordering};

struct AppState {
    directory: String,
    jwt_secret: String,
    submission_counter: AtomicU64,
    submission_size_total: AtomicU64,
    error_counter: AtomicU64,
    system_error_counter: AtomicU64,
    user_error_counter: AtomicU64,
    start_time: std::time::Instant,
}

fn verify_submission_path(path: &str) -> bool {
    let path = Path::new(path);
    path.exists() && path.is_dir()
}

fn wait_for_file(path: &str) -> bool {
    let path = Path::new(path);
    // wait for the file to be created
    for _ in 0..300 {
        if path.exists() {
            return true;
        }
        println!("Waiting for file {} to be created", path.display());
        std::thread::sleep(std::time::Duration::from_millis(1000));
    }
    false
}

async fn submission_metrics(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let spool_path = Path::new(&state.directory);
    let json_files_num = match spool_path.read_dir() {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and( |ext| ext == "json"))
            .count(),
        Err(_) => 0,
    };
    // Prometheus metrics format
    // String to hold the metrics
    let mut metrics = String::new();
    metrics.push_str("# HELP kcidb_submissions_total Total number of submissions received\n");
    metrics.push_str("# TYPE kcidb_submissions_total counter\n");
    metrics.push_str(&format!(
        "kcidb_submissions_total {}\n",
        state.submission_counter.load(Ordering::Relaxed)
    ));
    metrics.push_str(
        "# HELP kcidb_submission_size_total Total size of all submissions received in bytes\n",
    );
    metrics.push_str("# TYPE kcidb_submission_size_total counter\n");
    metrics.push_str(&format!(
        "kcidb_submission_size_total {}\n",
        state.submission_size_total.load(Ordering::Relaxed)
    ));
    metrics.push_str("# HELP kcidb_errors_total Total number of errors encountered\n");
    metrics.push_str("# TYPE kcidb_errors_total counter\n");
    metrics.push_str(&format!(
        "kcidb_errors_total {}\n",
        state.error_counter.load(Ordering::Relaxed)
    ));
    metrics.push_str("# HELP kcidb_system_errors_total Total number of system errors encountered\n");
    metrics.push_str("# TYPE kcidb_system_errors_total counter\n");
    metrics.push_str(&format!(
        "kcidb_system_errors_total {}\n",
        state.system_error_counter.load(Ordering::Relaxed)
    ));
    metrics.push_str("# HELP kcidb_user_errors_total Total number of user errors encountered\n");
    metrics.push_str("# TYPE kcidb_user_errors_total counter\n");
    metrics.push_str(&format!(
        "kcidb_user_errors_total {}\n",
        state.user_error_counter.load(Ordering::Relaxed)
    ));
    // number of json files in the spool directory
    metrics.push_str(
        "# HELP kcidb_json_files_total Total number of JSON files in the spool directory\n",
    );
    metrics.push_str("# TYPE kcidb_json_files_total gauge\n");
    metrics.push_str(&format!("kcidb_json_files_total {}\n", json_files_num));
    // Uptime in seconds
    let uptime = state.start_time.elapsed().as_secs();
    metrics.push_str("# HELP kcidb_uptime_seconds Uptime of the server in seconds\n");
    metrics.push_str("# TYPE kcidb_uptime_seconds gauge\n");
    metrics.push_str(&format!("kcidb_uptime_seconds {}\n", uptime));

    (StatusCode::OK, metrics)
}

fn are_we_root() -> bool {
    // Check if running as root (uid 0) without using unsafe
    #[cfg(target_family = "unix")]
    {
        match nix::unistd::Uid::effective().is_root() {
            true => true,
            false => false,
        }
    }
    #[cfg(not(target_family = "unix"))]
    {
        // On non-Unix platforms, always return false
        false
    }
}

async fn handle_root() -> impl IntoResponse {
    let index_path = Path::new("/usr/local/share/kcidb-restd-rs/index.html");
    let html = tokio::fs::read_to_string(index_path).await.unwrap_or_else(|_| {
        // Fallback HTML if the file is not found
        "<html><body><h1>Welcome to KCIDB REST API</h1></body></html>".to_string()
    });
    (
        StatusCode::OK,
        axum::response::Html(html)
    )
}

async fn auth_test(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let auth_result = verify_auth(headers, state.clone());
    match auth_result {
        Ok(jwt) => {
            println!("Authentication successful for origin: {}", jwt.origin);
        }
        Err(e) => {
            println!("Error: {}", e);
            state.user_error_counter.fetch_add(1, Ordering::Relaxed);
            let jsanswer = generate_answer("error", "0", Some(e));
            return (StatusCode::UNAUTHORIZED, jsanswer);
        }
    }
    let jsanswer = generate_answer("ok", "0", Some("Authentication successful".to_string()));
    (StatusCode::OK, jsanswer) 
}

#[tokio::main]
async fn main() {
    let limit_layer = RequestBodyLimitLayer::new(512 * 1024 * 1024);
    let args = Args::parse();
    let jwt_secret = std::env::var("JWT_SECRET").unwrap_or_else(|_| args.jwt_secret.clone());
    let app_state = Arc::new(AppState {
        directory: args.directory,
        jwt_secret,
        submission_counter: AtomicU64::new(0),
        submission_size_total: AtomicU64::new(0),
        error_counter: AtomicU64::new(0),
        system_error_counter: AtomicU64::new(0),
        user_error_counter: AtomicU64::new(0),
        start_time: std::time::Instant::now(),
    });
    let tls_key: String;
    let tls_chain: String;
    // print if JWT_SECRET is set in env
    if let Ok(_jwt_secret) = std::env::var("JWT_SECRET") {
        println!("Using JWT secret from environment variable");
    } else {
        println!("Using JWT secret from command line argument");
    }

    // do we have CERTBOT_DOMAIN? Then certificates are in /certs/live/${CERTBOT_DOMAIN}/
    // fullchain.pem and privkey.pem
    if let Ok(certbot_domain) = std::env::var("CERTBOT_DOMAIN") {
        tls_key = format!("/etc/letsencrypt/live/{}/privkey.pem", certbot_domain);
        tls_chain = format!("/etc/letsencrypt/live/{}/fullchain.pem", certbot_domain);
        // check if the file exists
        if wait_for_file(&tls_key) {
            println!(
                "Using TLS key from /etc/letsencrypt/live/{}/privkey.pem",
                certbot_domain
            );
        } else {
            eprintln!("Error: TLS key file {} does not exist", tls_key);
            std::process::exit(1);
        }
    } else {
        tls_key = String::new();
        tls_chain = String::new();
    }
    if !verify_submission_path(&app_state.directory) {
        eprintln!(
            "Error: submissions path {} does not exist or is not a directory",
            app_state.directory
        );
        std::process::exit(1);
    }
    // if default value - warn
    if app_state.jwt_secret == "secret" {
        eprintln!("Warning: JWT secret is default value");
    }
    // if secret is empty, warn
    if app_state.jwt_secret.is_empty() {
        eprintln!("Warning: JWT secret is empty, disabling authentication");
    }
    let mut port = args.port;

    // if we are not root, change if port < 1024 to 8080
    if port < 1024 && !are_we_root() {
        println!("Warning: Port {} is less than 1024, you dont have root, using 8080 instead", args.port);
        port = 8080;
    }

    if tls_key.is_empty() && port == 0 {
        port = 80;
    } else if port == 0 {
        port = 443;
    }

    println!(
        "Listening on {}:{}, submissions path: {}",
        args.host, port, app_state.directory
    );

    // plain http if tls_key is empty
    if tls_key.is_empty() {
        println!("Starting HTTP server");
        let app = Router::new()
            .route("/", get(handle_root))
            .route("/submit", post(receive_submission))
            .route("/submitlogs", post(receive_logs))
            .route("/status", get(submission_status))
            .route("/metrics", get(submission_metrics))
            .route("/health", get(|| async { "OK" }))
            .route("/authtest", get(auth_test))
            .with_state(app_state)
            .layer(limit_layer)
            .layer(axum::extract::DefaultBodyLimit::max(512 * 1024 * 1024));
        let tcp_listener = match TcpListener::bind((args.host.as_str(), port)).await {
            Ok(listener) => listener,
            Err(e) => {
                eprintln!("Failed to bind to {}:{}: {}", args.host, port, e);
                std::process::exit(1);
            }
        };
        if let Err(e) = axum::serve(tcp_listener, app).await {
            eprintln!("HTTP server failed: {}", e);
            std::process::exit(1);
        }
    } else {
        println!(
            "Starting HTTPS server with TLS key: {} and chain: {}",
            tls_key, tls_chain
        );
        let app = Router::new()
            .route("/", get(handle_root))
            .route("/submit", post(receive_submission))
            .route("/submitlogs", post(receive_logs))
            .route("/status", get(submission_status))
            .route("/metrics", get(submission_metrics))
            .route("/health", get(|| async { "OK" }))
            .route("/authtest", get(auth_test))
            .with_state(app_state)
            .layer(limit_layer)
            .layer(axum::extract::DefaultBodyLimit::max(512 * 1024 * 1024));
        //let tcp_listener = TcpListener::bind((args.host, args.port)).await.unwrap();
        let tls_config = match RustlsConfig::from_pem_file(tls_chain, tls_key).await {
            Ok(config) => config,
            Err(e) => {
                eprintln!("Failed to load TLS configuration: {}", e);
                std::process::exit(1);
            }
        };
        let address = format!("{}:{}", args.host, port);
        let addr = match address.parse::<std::net::SocketAddr>() {
            Ok(addr) => addr,
            Err(e) => {
                eprintln!("Failed to parse address {}: {}", address, e);
                std::process::exit(1);
            }
        };
        if let Err(e) = axum_server::bind_rustls(addr, tls_config)
            .serve(app.into_make_service())
            .await {
            eprintln!("HTTPS server failed: {}", e);
            std::process::exit(1);
        }
    }
}

fn verify_auth(headers: HeaderMap, state: Arc<AppState>) -> Result<JWT, String> {
    if state.jwt_secret.is_empty() {
        // If no secret, return a default JWT or handle as needed
        return Ok(JWT {
            origin: "none".to_string(),
            gendate: "none".to_string(),
        });
    }
    let jwt_r = headers.get("Authorization");
    let jwt = match jwt_r {
        Some(jwt) => jwt,
        None => return Err("JWT is required".to_string()),
    };
    let jwt_str_r = jwt.to_str();
    let jwt_str = match jwt_str_r {
        Ok(jwt_str) => jwt_str,
        Err(_) => return Err("Missing or invalid JWT".to_string()),
    };
    let jwt_str_r = jwt_str.split(" ").nth(1);
    let jwt_str = match jwt_str_r {
        Some(jwt_str) => jwt_str,
        None => return Err("Missing or invalid JWT (Bearer)".to_string()),
    };
    let jwt = verify_jwt(jwt_str, &state.jwt_secret);
    match jwt {
        Ok(jwt) => Ok(jwt),
        Err(e) => Err(e.to_string()),
    }
}

fn generate_answer(status: &str, id: &str, message: Option<String>) -> String {
    let status = SubmissionStatus {
        id: id.to_string(),
        status: status.to_string(),
        message,
    };
    // serialize to json
    serde_json::to_string(&status).unwrap().to_string()
}

#[derive(Debug, Serialize, Deserialize)]
struct LogUploadResponse {
    log_url: String,
}

fn storage_base_url() -> String {
    let base = std::env::var("KCIDB_STORAGE_URL")
        .unwrap_or_else(|_| "https://files.kernelci.org".to_string());
    base.trim_end_matches('/').to_string()
}

fn sanitize_filename(filename: &str) -> Result<String, String> {
    let base = Path::new(filename)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .trim();
    if base.is_empty() {
        return Err("Missing filename".to_string());
    }
    if base == "." || base == ".." {
        return Err("Invalid filename".to_string());
    }
    if !base.ends_with(".log.gz") {
        return Err("Filename must end with .log.gz".to_string());
    }
    let stem = &base[..base.len().saturating_sub(7)];
    if stem.is_empty() {
        return Err("Filename must start with [a-z0-9_]+".to_string());
    }
    if !stem
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err("Filename must match [a-z0-9_]+.log.gz".to_string());
    }
    Ok(base.to_string())
}

fn validate_origin(origin: &str) -> Result<(), String> {
    if origin.is_empty() {
        return Err("Empty origin".to_string());
    }
    if !origin
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err("Invalid origin".to_string());
    }
    Ok(())
}

fn normalize_log_filename(filename: &str) -> String {
    if filename.ends_with(".log.gz") {
        filename.to_string()
    } else {
        format!("{}.log.gz", filename)
    }
}

fn validate_submission_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("Empty submission_id".to_string());
    }
    if id.len() > 64 {
        return Err("submission_id too long".to_string());
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err("submission_id must match [a-z0-9_]+".to_string());
    }
    Ok(())
}


#[derive(serde::Deserialize)]
struct StatusQuery {
    id: String,
}
/*
/status?id=1234
*/
async fn submission_status(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    axum::extract::Query(query): axum::extract::Query<StatusQuery>,
) -> impl IntoResponse {
    let auth_result = verify_auth(headers, state.clone());
    match auth_result {
        Ok(jwt) => {
            println!("Authentication successful for origin: {}", jwt.origin);
        }
        Err(e) => {
            println!("Error: {}", e);
            state.user_error_counter.fetch_add(1, Ordering::Relaxed);
            let jsanswer = generate_answer("error", "0", Some(e));
            return (StatusCode::UNAUTHORIZED, jsanswer);
        }
    }
    let id = query.id;
    // validate id for safe characters
    if id.is_empty() {
        state.user_error_counter.fetch_add(1, Ordering::Relaxed);
        let jsanswer = generate_answer("error", "0", Some("Empty id".to_string()));
        return (StatusCode::BAD_REQUEST, jsanswer);
    }

    // id is alphanumeric
    if !id.chars().all(|c| c.is_alphanumeric()) {
        state.user_error_counter.fetch_add(1, Ordering::Relaxed);
        let jsanswer = generate_answer("error", "0", Some("Invalid id".to_string()));
        return (StatusCode::BAD_REQUEST, jsanswer);
    }

    let mut submission_file = format!("{}/submission-{}.json.temp", state.directory, id);
    // check if the file exists
    if Path::new(&submission_file).exists() {
        // check if the file is empty
        let jsanswer = generate_answer("inprogress", id.as_str(), Some("File still in progress".to_string()));
        return (StatusCode::OK, jsanswer)
    }

    submission_file = format!("{}/submission-{}.json", state.directory, id);
    // check if the submission file exists
    if Path::new(&submission_file).exists() {
        let jsanswer = generate_answer("ready", id.as_str(), Some("File waiting for processing".to_string()));
        return (StatusCode::OK, jsanswer);
    }

    submission_file = format!("{}/archive/submission-{}.json", state.directory, id);
    // check if the archived file exists
    if Path::new(&submission_file).exists() {
        let jsanswer = generate_answer("processed", id.as_str(), Some("File archived".to_string()));
        return (StatusCode::OK, jsanswer);
    }

    submission_file = format!("{}/failed/submission-{}.json", state.directory, id);
    // check if the failed file exists
    if Path::new(&submission_file).exists() {
        let jsanswer = generate_answer("failed", id.as_str(), Some("File failed to pass validation".to_string()));
        return (StatusCode::OK, jsanswer);
    }

    state.user_error_counter.fetch_add(1, Ordering::Relaxed);
    let jsanswer = generate_answer("notfound", id.as_str(), Some("File not found".to_string()));
    
    return (StatusCode::NOT_FOUND, jsanswer);
}

// Answer STATUS 200 if the submission is valid
async fn receive_submission(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    body: String,
) -> impl IntoResponse {
    let auth_result = verify_auth(headers, state.clone());
    let origin: String;
    match auth_result {
        Ok(jwt) => {
            origin = jwt.origin;
        }
        Err(e) => {
            println!("Error: {}", e);
            let err_status = SubmissionStatus {
                id: "0".to_string(),
                status: "error".to_string(),
                message: Some(e),
            };
            let err_json = serde_json::to_string(&err_status).unwrap();
            // increment error counter atomically
            state.error_counter.fetch_add(1, Ordering::Relaxed);

            return (StatusCode::UNAUTHORIZED, err_json);
        }
    }

    let submission_json = serde_json::from_str::<serde_json::Value>(&body);
    match submission_json {
        Ok(_submission) => {
            let size = body.len();
            println!("Received submission size: {} from {} ", size, origin);
            let submission_id = random_string(32);
            let submission_file =
                format!("{}/submission-{}.json.temp", state.directory, submission_id);
            if let Err(e) = std::fs::write(&submission_file, &body) {
                eprintln!("Failed to write submission file: {}", e);
                state.system_error_counter.fetch_add(1, Ordering::Relaxed);
                let err_status = SubmissionStatus {
                    id: "0".to_string(),
                    status: "error".to_string(),
                    message: Some("Internal server error".to_string()),
                };
                let err_json = serde_json::to_string(&err_status).unwrap_or_default();
                return (StatusCode::INTERNAL_SERVER_ERROR, err_json);
            }
            // on completion, rename to submission.json
            if let Err(e) = std::fs::rename(
                &submission_file,
                format!("{}/submission-{}.json", state.directory, submission_id),
            ) {
                eprintln!("Failed to rename submission file: {}", e);
                state.system_error_counter.fetch_add(1, Ordering::Relaxed);
                let err_status = SubmissionStatus {
                    id: "0".to_string(),
                    status: "error".to_string(),
                    message: Some("Internal server error".to_string()),
                };
                let err_json = serde_json::to_string(&err_status).unwrap_or_default();
                return (StatusCode::INTERNAL_SERVER_ERROR, err_json);
            }
            println!("Submission {} received", submission_id);
            let msg = format!(
                "Received submission {} with size {} bytes",
                submission_id, size
            );

            let status = SubmissionStatus {
                id: submission_id,
                status: "ok".to_string(),
                message: Some(msg),
            };
            let jsonstr = serde_json::to_string(&status).unwrap();
            // increment submission counter atomically
            state.submission_counter.fetch_add(1, Ordering::Relaxed);
            state
                .submission_size_total
                .fetch_add(size as u64, Ordering::Relaxed);
            println!("Submission status: {}", jsonstr);
            (StatusCode::OK, jsonstr)
        }
        Err(e) => {
            println!("Error: {}", e);
            state.user_error_counter.fetch_add(1, Ordering::Relaxed);
            let err_status = SubmissionStatus {
                id: "0".to_string(),
                status: "error".to_string(),
                message: Some(e.to_string()),
            };
            let err_json = serde_json::to_string(&err_status).unwrap();
            (StatusCode::BAD_REQUEST, err_json)
        }
    }
}

// POST /submitlogs multipart/form-data
async fn receive_logs(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> impl IntoResponse {
    let auth_result = verify_auth(headers, state.clone());
    let origin: String;
    match auth_result {
        Ok(jwt) => {
            origin = jwt.origin;
        }
        Err(e) => {
            println!("Error: {}", e);
            let err_status = SubmissionStatus {
                id: "0".to_string(),
                status: "error".to_string(),
                message: Some(e),
            };
            let err_json = serde_json::to_string(&err_status).unwrap();
            state.error_counter.fetch_add(1, Ordering::Relaxed);
            return (StatusCode::UNAUTHORIZED, err_json);
        }
    }

    if let Err(e) = validate_origin(&origin) {
        state.user_error_counter.fetch_add(1, Ordering::Relaxed);
        let err_status = SubmissionStatus {
            id: "0".to_string(),
            status: "error".to_string(),
            message: Some(e),
        };
        let err_json = serde_json::to_string(&err_status).unwrap();
        return (StatusCode::BAD_REQUEST, err_json);
    }

    let mut submission_id: Option<String> = None;
    let mut file_bytes: Option<Bytes> = None;
    let mut file_name: Option<String> = None;
    let mut file_length_header: Option<u64> = None;

    while let Some(field) = match multipart.next_field().await {
        Ok(field) => field,
        Err(e) => {
            println!("Error: {}", e);
            state.user_error_counter.fetch_add(1, Ordering::Relaxed);
            let err_status = SubmissionStatus {
                id: "0".to_string(),
                status: "error".to_string(),
                message: Some("Invalid multipart form data".to_string()),
            };
            let err_json = serde_json::to_string(&err_status).unwrap();
            return (StatusCode::BAD_REQUEST, err_json);
        }
    } {
        if let Some(name) = field.name() {
            if name == "submission_id" || name == "submissionid" {
                if let Ok(text) = field.text().await {
                    submission_id = Some(text.trim().to_string());
                }
                continue;
            }
        }

        if field.name() == Some("log") && field.file_name().is_some() {
            if file_bytes.is_none() {
                file_name = field.file_name().map(|s| s.to_string());
                if let Some(header_value) = field.headers().get(CONTENT_LENGTH) {
                    if let Ok(header_str) = header_value.to_str() {
                        if let Ok(value) = header_str.parse::<u64>() {
                            file_length_header = Some(value);
                        }
                    }
                }
                match field.bytes().await {
                    Ok(bytes) => file_bytes = Some(bytes),
                    Err(_) => {
                        state.user_error_counter.fetch_add(1, Ordering::Relaxed);
                        let err_status = SubmissionStatus {
                            id: "0".to_string(),
                            status: "error".to_string(),
                            message: Some("Failed to read uploaded file".to_string()),
                        };
                        let err_json = serde_json::to_string(&err_status).unwrap();
                        return (StatusCode::BAD_REQUEST, err_json);
                    }
                }
            }
        }
    }

    let submission_id = match submission_id {
        Some(id) => id,
        None => {
            state.user_error_counter.fetch_add(1, Ordering::Relaxed);
            let err_status = SubmissionStatus {
                id: "0".to_string(),
                status: "error".to_string(),
                message: Some("submission_id is required".to_string()),
            };
            let err_json = serde_json::to_string(&err_status).unwrap();
            return (StatusCode::BAD_REQUEST, err_json);
        }
    };

    if let Err(e) = validate_submission_id(&submission_id) {
        state.user_error_counter.fetch_add(1, Ordering::Relaxed);
        let err_status = SubmissionStatus {
            id: "0".to_string(),
            status: "error".to_string(),
            message: Some(e),
        };
        let err_json = serde_json::to_string(&err_status).unwrap();
        return (StatusCode::BAD_REQUEST, err_json);
    }

    let file_name = match file_name {
        Some(name) => name,
        None => {
            state.user_error_counter.fetch_add(1, Ordering::Relaxed);
            let err_status = SubmissionStatus {
                id: "0".to_string(),
                status: "error".to_string(),
                message: Some("log file is required".to_string()),
            };
            let err_json = serde_json::to_string(&err_status).unwrap();
            return (StatusCode::BAD_REQUEST, err_json);
        }
    };

    let sanitized = match sanitize_filename(&file_name) {
        Ok(name) => name,
        Err(e) => {
            state.user_error_counter.fetch_add(1, Ordering::Relaxed);
            let err_status = SubmissionStatus {
                id: "0".to_string(),
                status: "error".to_string(),
                message: Some(e),
            };
            let err_json = serde_json::to_string(&err_status).unwrap();
            return (StatusCode::BAD_REQUEST, err_json);
        }
    };
    let normalized_name = normalize_log_filename(&sanitized);

    let file_bytes = match file_bytes {
        Some(bytes) => bytes,
        None => {
            state.user_error_counter.fetch_add(1, Ordering::Relaxed);
            let err_status = SubmissionStatus {
                id: "0".to_string(),
                status: "error".to_string(),
                message: Some("log file is required".to_string()),
            };
            let err_json = serde_json::to_string(&err_status).unwrap();
            return (StatusCode::BAD_REQUEST, err_json);
        }
    };
    if let Some(expected) = file_length_header {
        if file_bytes.len() as u64 != expected {
            state.user_error_counter.fetch_add(1, Ordering::Relaxed);
            let err_status = SubmissionStatus {
                id: "0".to_string(),
                status: "error".to_string(),
                message: Some("Log Content-Length mismatch".to_string()),
            };
            let err_json = serde_json::to_string(&err_status).unwrap();
            return (StatusCode::BAD_REQUEST, err_json);
        }
    }

    let logs_dir = format!("{}/logs", state.directory);
    if let Err(e) = std::fs::create_dir_all(&logs_dir) {
        eprintln!("Failed to create logs directory: {}", e);
        state.system_error_counter.fetch_add(1, Ordering::Relaxed);
        let err_status = SubmissionStatus {
            id: "0".to_string(),
            status: "error".to_string(),
            message: Some("Internal server error".to_string()),
        };
        let err_json = serde_json::to_string(&err_status).unwrap_or_default();
        return (StatusCode::INTERNAL_SERVER_ERROR, err_json);
    }

    let local_filename = format!("{}_{}_{}", origin, submission_id, normalized_name);
    let temp_path = format!("{}/{}.temp", logs_dir, local_filename);
    let final_path = format!("{}/{}", logs_dir, local_filename);

    if Path::new(&final_path).exists() || Path::new(&temp_path).exists() {
        state.user_error_counter.fetch_add(1, Ordering::Relaxed);
        let err_status = SubmissionStatus {
            id: "0".to_string(),
            status: "error".to_string(),
            message: Some("log file already exists".to_string()),
        };
        let err_json = serde_json::to_string(&err_status).unwrap();
        return (StatusCode::CONFLICT, err_json);
    }

    if let Err(e) = std::fs::write(&temp_path, &file_bytes) {
        eprintln!("Failed to write log file: {}", e);
        state.system_error_counter.fetch_add(1, Ordering::Relaxed);
        let err_status = SubmissionStatus {
            id: "0".to_string(),
            status: "error".to_string(),
            message: Some("Internal server error".to_string()),
        };
        let err_json = serde_json::to_string(&err_status).unwrap_or_default();
        return (StatusCode::INTERNAL_SERVER_ERROR, err_json);
    }
    if let Err(e) = std::fs::rename(&temp_path, &final_path) {
        eprintln!("Failed to rename log file: {}", e);
        state.system_error_counter.fetch_add(1, Ordering::Relaxed);
        let err_status = SubmissionStatus {
            id: "0".to_string(),
            status: "error".to_string(),
            message: Some("Internal server error".to_string()),
        };
        let err_json = serde_json::to_string(&err_status).unwrap_or_default();
        return (StatusCode::INTERNAL_SERVER_ERROR, err_json);
    }

    let log_url = format!(
        "{}/{}/{}/{}",
        storage_base_url(),
        origin,
        submission_id,
        normalized_name
    );
    let response = LogUploadResponse { log_url };
    let jsonstr = serde_json::to_string(&response).unwrap();
    (StatusCode::OK, jsonstr)
}

#[derive(Debug, Serialize, Deserialize)]
struct JWT {
    origin: String,
    gendate: String,
}

fn verify_jwt(token: &str, secret: &str) -> Result<JWT, jsonwebtoken::errors::Error> {
    let key = DecodingKey::from_secret(secret.as_bytes());
    let token = decode::<JWT>(token, &key, &Validation::default())?;
    Ok(token.claims)
}

/* STUB for now */
/*
fn generate_jwt(origin: &str, gendate: &str, secret: &str) -> Result<String, jsonwebtoken::errors::Error> {
    let key = EncodingKey::from_secret(secret.as_bytes());
    let token = encode(&Header::default(), &JWT { origin: origin.to_string(), gendate: gendate.to_string() }, &key)?;
    Ok(token)
}
*/

// TODO: Fix this
fn random_string(length: usize) -> String {
    let mut rng = rand::rng();
    // rng.sample(rand::distr::Alphanumeric) as char
    let mut s = String::new();
    for _ in 0..length {
        s.push(rng.sample(rand::distr::Alphanumeric) as char);
    }
    s
}
