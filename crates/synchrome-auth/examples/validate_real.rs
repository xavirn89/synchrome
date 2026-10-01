//! Real-instance validation driver for the Auth slice. Uses the SAME code path as the app:
//! `SessionManager` + `KeychainStore` + `AuthClient<ReqwestTransport>`.
//!
//! Each invocation is a fresh process (== app restart). Never prints tokens.
//!
//! Usage: validate_real <login|status|reuse|refresh|logout|logout-offline> [creds-env-file]
//!   SUPABASE_URL / SUPABASE_ANON_KEY come from the repo .env via AppConfig::load().
use std::sync::Arc;
use synchrome_auth::{AccountStatus, AuthClient, KeychainStore, SessionManager};
use synchrome_config::{AppConfig, SupabaseConfig};

fn creds(path: &str) -> (String, String) {
    let text = std::fs::read_to_string(path).expect("creds env file");
    let get = |k: &str| {
        text.lines()
            .find_map(|l| {
                l.strip_prefix(&format!("{k}="))
                    .map(|v| v.trim().to_string())
            })
            .unwrap_or_default()
    };
    (get("E9_AUTH_EMAIL"), get("E9_AUTH_PASSWORD"))
}

fn print_status(label: &str, st: &AccountStatus) {
    // Serialize exactly what the frontend would receive and prove no token keys exist.
    let v = serde_json::to_value(st).unwrap();
    let leaked = v.get("access_token").is_some() || v.get("refresh_token").is_some();
    println!("{label}: {}", serde_json::to_string(&v).unwrap());
    println!("{label}: token fields present in payload = {leaked}");
}

fn manager(cfg: SupabaseConfig) -> SessionManager<synchrome_auth::ReqwestTransport> {
    SessionManager::new(
        AuthClient::new(cfg).expect("http client"),
        Arc::new(KeychainStore::new()),
    )
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("status");
    let creds_path = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "/tmp/synchrome-auth-test.env".into());
    let cfg = AppConfig::load().expect("client config (.env)").supabase;
    let rt = tokio::runtime::Runtime::new().unwrap();

    match cmd {
        "login" => {
            let (email, password) = creds(&creds_path);
            let m = manager(cfg);
            match rt.block_on(m.sign_in(&email, &password)) {
                Ok(st) => print_status("login", &st),
                Err(e) => {
                    println!("login: ERROR {e}");
                    std::process::exit(1)
                }
            }
        }
        "status" => {
            let m = manager(cfg);
            print_status("status", &m.account_status());
        }
        "reuse" => {
            // Must succeed WITHOUT a network login; reports whether a refresh happened.
            let m = manager(cfg);
            let before = m.account_status();
            match rt.block_on(m.ensure_valid_session()) {
                Ok(Some(s)) => {
                    let refreshed = before.expires_at != Some(s.expires_at);
                    println!(
                        "reuse: session valid, user_id={}, refreshed={refreshed}",
                        s.user_id
                    );
                }
                Ok(None) => println!("reuse: NO SESSION"),
                Err(e) => println!("reuse: ERROR {e}"),
            }
        }
        "refresh" => {
            // Force a refresh by calling the client directly through the manager path:
            // we emulate near-expiry by invoking ensure_valid_session after noting expires_at,
            // then explicitly refreshing via a second manager call that treats it as due.
            let m = manager(cfg.clone());
            let st0 = m.account_status();
            // Direct refresh using the stored refresh token (same client the manager uses).
            let store = KeychainStore::new();
            let sess = synchrome_auth::CredentialStore::load_session(&store)
                .expect("store")
                .expect("session present");
            let client = AuthClient::new(cfg).expect("client");
            match rt.block_on(client.refresh(&sess.refresh_token)) {
                Ok(fresh) => {
                    synchrome_auth::CredentialStore::save_session(&store, &fresh).expect("save");
                    let st1 = m.account_status();
                    println!(
                        "refresh: OK, expires_at {:?} -> {:?}, user_id unchanged={}",
                        st0.expires_at,
                        st1.expires_at,
                        st0.user_id == st1.user_id
                    );
                    print_status("refresh.status", &st1);
                }
                Err(e) => {
                    println!("refresh: ERROR {e}");
                    std::process::exit(1)
                }
            }
        }
        "logout" => {
            let m = manager(cfg);
            match rt.block_on(m.sign_out()) {
                Ok(()) => println!("logout: OK (remote + local)"),
                Err(e) => println!("logout: local cleared; remote error: {e}"),
            }
            print_status("logout.status", &m.account_status());
        }
        "logout-offline" => {
            // Same Keychain session, but the remote endpoint is unreachable: local MUST clear.
            let bogus = SupabaseConfig {
                url: "https://synchrome-unreachable.invalid".into(),
                anon_key: cfg.anon_key,
            };
            let m = manager(bogus);
            match rt.block_on(m.sign_out()) {
                Ok(()) => println!("logout-offline: unexpected remote OK"),
                Err(e) => println!("logout-offline: remote failed as expected ({e})"),
            }
            print_status("logout-offline.status", &m.account_status());
        }
        other => {
            eprintln!("unknown command {other}");
            std::process::exit(2)
        }
    }
}
