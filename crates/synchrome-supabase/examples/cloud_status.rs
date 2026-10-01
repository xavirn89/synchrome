//! CLI check: `cargo run -p synchrome-supabase --example cloud_status`
//! Loads config from env/.env and prints the read-only probe result as JSON.
fn main() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let status = match synchrome_config::AppConfig::load() {
        Err(e) => synchrome_supabase::CloudStatus::unconfigured(e.to_string()),
        Ok(cfg) => rt
            .block_on(synchrome_supabase::cloud_status(&cfg.supabase))
            .unwrap_or_else(|e| synchrome_supabase::CloudStatus::unconfigured(e.to_string())),
    };
    println!("{}", serde_json::to_string_pretty(&status).unwrap());
}
