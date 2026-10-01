//! Real-instance validation for M1 slice 4 (device identity + account metadata).
//!
//! Uses ONLY normal login + anon key + each user's own JWT. No service role, no DB access.
//! Main flows go through the app's own code (`AuthClient`, `CloudClient`, `synchrome-device`);
//! operations the client does not expose yet (profile create/delete, B's attacks on A's rows,
//! cleanup) use raw PostgREST calls with the acting user's own JWT.
//! Never prints credentials or tokens.
//!
//! Usage:
//!   validate_slice4 device-id                 # print the persisted local device_id
//!   validate_slice4 run [creds-env-file]      # full validation + cleanup
use std::time::Duration;
use synchrome_auth::{AuthClient, Session};
use synchrome_cloud::{auth_headers, CloudClient, CloudError, DeviceSummary, ProfileSummary};
use synchrome_config::{AppConfig, SupabaseConfig};
use synchrome_device::DeviceIdentity;
use synchrome_http::HttpClient;
use uuid::Uuid;

struct Results(Vec<(String, bool)>);
impl Results {
    fn check(&mut self, name: &str, ok: bool, detail: impl AsRef<str>) -> bool {
        println!(
            "  [{}] {name} — {}",
            if ok { "PASS" } else { "FAIL" },
            detail.as_ref()
        );
        self.0.push((name.to_string(), ok));
        ok
    }
}

fn creds(path: &str) -> [String; 4] {
    let text = std::fs::read_to_string(path).expect("creds file");
    let get = |k: &str| {
        text.lines()
            .find_map(|l| {
                l.strip_prefix(&format!("{k}="))
                    .map(|v| v.trim().to_string())
            })
            .unwrap_or_default()
    };
    [
        get("USER_A_EMAIL"),
        get("USER_A_PASSWORD"),
        get("USER_B_EMAIL"),
        get("USER_B_PASSWORD"),
    ]
}

/// Raw PostgREST call with the acting user's own JWT. Returns (status, body).
async fn raw(
    cfg: &SupabaseConfig,
    http: &HttpClient,
    s: &Session,
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> (u16, String) {
    let owned = auth_headers(cfg, s);
    let mut h: Vec<(&str, &str)> = owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    h.push(("Prefer", "return=representation"));
    h.push(("Content-Type", "application/json"));
    let url = format!("{}/rest/v1{}", cfg.url.trim_end_matches('/'), path);
    match http.request_json(method, &url, &h, body.as_ref()).await {
        Ok((st, b)) => (st, String::from_utf8_lossy(&b).into_owned()),
        Err(e) => (0, e.to_string()),
    }
}

fn rows(body: &str) -> Vec<serde_json::Value> {
    serde_json::from_str(body).unwrap_or_default()
}

fn find(list: &[DeviceSummary], id: Uuid) -> Option<&DeviceSummary> {
    list.iter().find(|d| d.device_id == id)
}

async fn run(path: &str) -> bool {
    let cfg = AppConfig::load().expect(".env client config").supabase;
    let http = HttpClient::new().unwrap();
    let auth = AuthClient::new(cfg.clone()).unwrap();
    let cloud = CloudClient::new(cfg.clone()).unwrap();
    let [a_email, a_pw, b_email, b_pw] = creds(path);
    let mut r = Results(vec![]);
    let tag = Uuid::new_v4().simple().to_string()[..8].to_string();

    // ---------- 1. login A ----------
    let sa = match auth.login_password(&a_email, &a_pw).await {
        Ok(s) => {
            r.check("1 login A", true, format!("user_id {}", s.user_id));
            s
        }
        Err(e) => {
            r.check("1 login A", false, e.to_string());
            return false;
        }
    };

    // ---------- 2. register A's device (the real local identity) ----------
    let me = synchrome_device::load_or_create().expect("local device identity");
    println!(
        "  local device_id (this installation, used by A) = {}",
        me.device_id
    );
    let d1 = cloud.upsert_device(&sa, &me).await;
    let d1_ok = matches!(&d1, Ok(d) if d.device_id == me.device_id && d.is_current);
    r.check(
        "2 register_device creates A's device",
        d1_ok,
        format!("{:?}", d1.as_ref().map(|d| (&d.name, &d.last_seen_at))),
    );
    let d1 = match d1 {
        Ok(d) => d,
        Err(_) => return false,
    };

    // ---------- 3. second register: no duplicate, last_seen_at + metadata updated ----------
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let d2 = cloud.upsert_device(&sa, &me).await.expect("register #2");
    let la = cloud.list_devices(&sa, me.device_id).await.expect("list A");
    let n = la.iter().filter(|d| d.device_id == me.device_id).count();
    r.check(
        "3a second register_device does not duplicate",
        n == 1,
        format!("rows with this device_id = {n}"),
    );
    r.check(
        "3b second register_device advances last_seen_at",
        d2.last_seen_at > d1.last_seen_at,
        format!("{:?} -> {:?}", d1.last_seen_at, d2.last_seen_at),
    );
    let mut renamed = me.clone();
    renamed.device_name = format!("{} [slice4 rename check]", me.device_name);
    let d3 = cloud
        .upsert_device(&sa, &renamed)
        .await
        .expect("register #3");
    let la = cloud.list_devices(&sa, me.device_id).await.expect("list A");
    r.check(
        "3c re-register updates metadata in place (merge-duplicates)",
        d3.name == renamed.device_name
            && la.iter().filter(|d| d.device_id == me.device_id).count() == 1,
        format!("name now {:?}", d3.name),
    );
    cloud.upsert_device(&sa, &me).await.expect("restore name");
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let t = cloud.touch_device(&sa, me.device_id).await.expect("touch");
    r.check(
        "3d touch_device advances last_seen_at",
        t.last_seen_at > d3.last_seen_at,
        format!("{:?} -> {:?}", d3.last_seen_at, t.last_seen_at),
    );

    // ---------- 4. list A: only A's, exactly one current ----------
    let la = cloud.list_devices(&sa, me.device_id).await.expect("list A");
    let current = la.iter().filter(|d| d.is_current).count();
    r.check(
        "4 list_devices(A) returns only A's devices, exactly one is_current",
        la.len() == 1 && current == 1 && la[0].device_id == me.device_id,
        format!("count {}, current {}", la.len(), current),
    );

    // ---------- 5. profiles A empty ----------
    let pa0 = cloud.list_profiles(&sa).await;
    r.check(
        "5 list_profiles(A) returns [] for a fresh account",
        matches!(&pa0, Ok(v) if v.is_empty()),
        format!("{:?}", pa0.as_ref().map(|v| v.len())),
    );

    // ---------- 6. login B ----------
    let sb = match auth.login_password(&b_email, &b_pw).await {
        Ok(s) => {
            r.check(
                "6 login B",
                s.user_id != sa.user_id,
                format!("user_id {} (≠ A)", s.user_id),
            );
            s
        }
        Err(e) => {
            r.check("6 login B", false, e.to_string());
            cleanup(&cfg, &http, &sa, None, me.device_id, None, None, &mut r).await;
            return false;
        }
    };
    let me_b = DeviceIdentity {
        device_id: Uuid::new_v4(),
        device_name: format!("slice4-test-device-B-{tag}"),
        platform: me.platform.clone(),
        os_version: None,
        app_version: me.app_version.clone(),
    };
    let db = cloud.upsert_device(&sb, &me_b).await;
    r.check(
        "6b B registers its own device",
        matches!(&db, Ok(d) if d.device_id == me_b.device_id),
        format!("{:?}", db.as_ref().map(|d| d.device_id)),
    );

    // ---------- 7. RLS ----------
    let lb = cloud
        .list_devices(&sb, me_b.device_id)
        .await
        .expect("list B");
    r.check(
        "7a B does not see A's devices",
        find(&lb, me.device_id).is_none() && lb.len() == 1,
        format!(
            "B sees {} device(s), A's present: {}",
            lb.len(),
            find(&lb, me.device_id).is_some()
        ),
    );

    let before_a = find(
        &cloud.list_devices(&sa, me.device_id).await.unwrap(),
        me.device_id,
    )
    .cloned()
    .unwrap();
    let tb = cloud.touch_device(&sb, me.device_id).await;
    let (st, body) = raw(
        &cfg,
        &http,
        &sb,
        "PATCH",
        &format!("/devices?device_id=eq.{}", me.device_id),
        Some(serde_json::json!({ "name": "pwned-by-B" })),
    )
    .await;
    let hijack = cloud
        .upsert_device(
            &sb,
            &DeviceIdentity {
                device_id: me.device_id,
                device_name: "hijacked-by-B".into(),
                ..me_b.clone()
            },
        )
        .await;
    let after_a = find(
        &cloud.list_devices(&sa, me.device_id).await.unwrap(),
        me.device_id,
    )
    .cloned()
    .unwrap();
    r.check(
        "7b B cannot modify A's device (PATCH/touch/upsert-hijack)",
        matches!(tb, Err(CloudError::InvalidResponse(_)))
            && st == 200
            && rows(&body).is_empty()
            && hijack.is_err()
            && after_a.name == before_a.name
            && after_a.last_seen_at == before_a.last_seen_at,
        format!(
            "touch→{}, PATCH→{st} rows={}, upsert-hijack→{}, A's row unchanged={}",
            tb.as_ref()
                .err()
                .map(|e| e.to_string())
                .unwrap_or("Ok?!".into()),
            rows(&body).len(),
            hijack
                .as_ref()
                .err()
                .map(|e| e.to_string())
                .unwrap_or("Ok?!".into()),
            after_a.name == before_a.name && after_a.last_seen_at == before_a.last_seen_at
        ),
    );

    let (st, body) = raw(
        &cfg,
        &http,
        &sb,
        "DELETE",
        &format!("/devices?device_id=eq.{}", me.device_id),
        None,
    )
    .await;
    let still = find(
        &cloud.list_devices(&sa, me.device_id).await.unwrap(),
        me.device_id,
    )
    .is_some();
    r.check(
        "7c B cannot delete A's device",
        st == 200 && rows(&body).is_empty() && still,
        format!(
            "DELETE→{st} rows={}, A's row still present={still}",
            rows(&body).len()
        ),
    );

    let la = cloud.list_devices(&sa, me.device_id).await.unwrap();
    r.check(
        "7d A does not see B's devices",
        find(&la, me_b.device_id).is_none(),
        format!("A sees {} device(s)", la.len()),
    );

    // ---------- 8. profiles (temporary, own JWT) + isolation ----------
    let (sta, ba) = raw(
        &cfg,
        &http,
        &sa,
        "POST",
        "/profiles",
        Some(serde_json::json!({ "name": format!("slice4-A-{tag}") })),
    )
    .await;
    let (stb, bb) = raw(
        &cfg,
        &http,
        &sb,
        "POST",
        "/profiles",
        Some(serde_json::json!({ "name": format!("slice4-B-{tag}") })),
    )
    .await;
    let pa_id = rows(&ba)
        .first()
        .and_then(|v| v["id"].as_str().map(String::from));
    let pb_id = rows(&bb)
        .first()
        .and_then(|v| v["id"].as_str().map(String::from));
    let server_sent_account_id = rows(&ba)
        .first()
        .map(|v| v.get("account_id").is_some())
        .unwrap_or(false);
    r.check(
        "8a each user creates a temporary profile with own JWT",
        sta == 201 && stb == 201 && pa_id.is_some() && pb_id.is_some(),
        format!("A→{sta}, B→{stb}"),
    );
    let pa = cloud.list_profiles(&sa).await.unwrap();
    let pb = cloud.list_profiles(&sb).await.unwrap();
    r.check(
        "8b profiles isolated: A sees only A's, B sees only B's",
        pa.len() == 1
            && pb.len() == 1
            && Some(pa[0].id.to_string()) == pa_id
            && Some(pb[0].id.to_string()) == pb_id,
        format!(
            "A sees {:?}, B sees {:?}",
            pa.iter().map(|p| &p.name).collect::<Vec<_>>(),
            pb.iter().map(|p| &p.name).collect::<Vec<_>>()
        ),
    );
    let a_pid = pa_id.clone().unwrap_or_default();
    let (s1, b1) = raw(
        &cfg,
        &http,
        &sb,
        "GET",
        &format!("/profiles?id=eq.{a_pid}"),
        None,
    )
    .await;
    let (s2, b2) = raw(
        &cfg,
        &http,
        &sb,
        "PATCH",
        &format!("/profiles?id=eq.{a_pid}"),
        Some(serde_json::json!({ "name": "pwned-by-B" })),
    )
    .await;
    let (s3, b3) = raw(
        &cfg,
        &http,
        &sb,
        "DELETE",
        &format!("/profiles?id=eq.{a_pid}"),
        None,
    )
    .await;
    let pa_after = cloud.list_profiles(&sa).await.unwrap();
    r.check(
        "8c B cannot read / rename / delete A's profile",
        s1 == 200
            && rows(&b1).is_empty()
            && s2 == 200
            && rows(&b2).is_empty()
            && s3 == 200
            && rows(&b3).is_empty()
            && pa_after.len() == 1
            && pa_after[0].name == format!("slice4-A-{tag}"),
        format!(
            "GET→{s1}/{} PATCH→{s2}/{} DELETE→{s3}/{}; A's profile intact={}",
            rows(&b1).len(),
            rows(&b2).len(),
            rows(&b3).len(),
            pa_after
                .first()
                .map(|p| p.name == format!("slice4-A-{tag}"))
                .unwrap_or(false)
        ),
    );

    // ---------- 9. frontend payload safety ----------
    let all_devices: Vec<DeviceSummary> = la.iter().chain(lb.iter()).cloned().collect();
    let all_profiles: Vec<ProfileSummary> = pa.iter().chain(pb.iter()).cloned().collect();
    let payload = format!(
        "{}{}",
        serde_json::to_string(&all_devices).unwrap(),
        serde_json::to_string(&all_profiles).unwrap()
    );
    let leak = payload.contains("account_id")
        || payload.contains(sa.access_token.expose())
        || payload.contains(sa.refresh_token.expose())
        || payload.contains(sb.access_token.expose())
        || payload.contains(sb.refresh_token.expose())
        || payload.contains("access_token")
        || payload.contains("refresh_token");
    r.check(
        "9 DeviceSummary/ProfileSummary carry no account_id and no tokens",
        !leak,
        format!(
            "server row included account_id={server_sent_account_id}, dropped by safe models={}",
            !payload.contains("account_id")
        ),
    );

    // ---------- 11. cleanup (own JWTs only) ----------
    cleanup(
        &cfg,
        &http,
        &sa,
        Some(&sb),
        me.device_id,
        Some(me_b.device_id),
        Some((pa_id, pb_id)),
        &mut r,
    )
    .await;

    let failed: Vec<_> =
        r.0.iter()
            .filter(|(_, ok)| !ok)
            .map(|(n, _)| n.clone())
            .collect();
    println!(
        "\n=== slice 4 summary: {} pass, {} fail ===",
        r.0.len() - failed.len(),
        failed.len()
    );
    if !failed.is_empty() {
        println!("  failures: {}", failed.join(", "));
    }
    failed.is_empty()
}

#[allow(clippy::too_many_arguments)]
async fn cleanup(
    cfg: &SupabaseConfig,
    http: &HttpClient,
    sa: &Session,
    sb: Option<&Session>,
    a_dev: Uuid,
    b_dev: Option<Uuid>,
    profiles: Option<(Option<String>, Option<String>)>,
    r: &mut Results,
) {
    let mut deleted = vec![];
    if let Some((pa, pb)) = profiles {
        if let Some(id) = pa {
            let (_, b) = raw(
                cfg,
                http,
                sa,
                "DELETE",
                &format!("/profiles?id=eq.{id}"),
                None,
            )
            .await;
            deleted.push(("A profile", rows(&b).len()));
        }
        if let (Some(id), Some(sb)) = (pb, sb) {
            let (_, b) = raw(
                cfg,
                http,
                sb,
                "DELETE",
                &format!("/profiles?id=eq.{id}"),
                None,
            )
            .await;
            deleted.push(("B profile", rows(&b).len()));
        }
    }
    let (_, b) = raw(
        cfg,
        http,
        sa,
        "DELETE",
        &format!("/devices?device_id=eq.{a_dev}"),
        None,
    )
    .await;
    deleted.push(("A device", rows(&b).len()));
    if let (Some(id), Some(sb)) = (b_dev, sb) {
        let (_, b) = raw(
            cfg,
            http,
            sb,
            "DELETE",
            &format!("/devices?device_id=eq.{id}"),
            None,
        )
        .await;
        deleted.push(("B device", rows(&b).len()));
    }
    // Verify both accounts are empty again.
    let (_, ad) = raw(cfg, http, sa, "GET", "/devices?select=device_id", None).await;
    let (_, ap) = raw(cfg, http, sa, "GET", "/profiles?select=id", None).await;
    let (bd, bp) = match sb {
        Some(sb) => (
            rows(
                &raw(cfg, http, sb, "GET", "/devices?select=device_id", None)
                    .await
                    .1,
            )
            .len(),
            rows(
                &raw(cfg, http, sb, "GET", "/profiles?select=id", None)
                    .await
                    .1,
            )
            .len(),
        ),
        None => (0, 0),
    };
    let empty = rows(&ad).is_empty() && rows(&ap).is_empty() && bd == 0 && bp == 0;
    r.check(
        "11 cleanup removed all test rows (own JWTs)",
        empty,
        format!(
            "deleted {deleted:?}; remaining A devices/profiles {}/{}, B {bd}/{bp}",
            rows(&ad).len(),
            rows(&ap).len()
        ),
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str).unwrap_or("run") {
        "device-id" => {
            let d = synchrome_device::load_or_create().expect("device identity");
            println!("device_id={}", d.device_id);
        }
        "run" => {
            let path = args
                .get(2)
                .cloned()
                .unwrap_or_else(|| "/tmp/synchrome-slice4-users.env".into());
            let ok = tokio::runtime::Runtime::new().unwrap().block_on(run(&path));
            std::process::exit(if ok { 0 } else { 1 });
        }
        other => {
            eprintln!("unknown command {other}");
            std::process::exit(2);
        }
    }
}
