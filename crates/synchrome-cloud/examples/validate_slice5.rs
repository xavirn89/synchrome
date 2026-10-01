//! Real-instance validation for M1 slice 5 (cloud profiles + local <-> cloud mapping).
//!
//! Only normal login + anon key + each user's own JWT. No service role, no DB access.
//! Main flows use the app's own code (`AuthClient`, `CloudClient`, `synchrome-device`,
//! `synchrome-browser` discovery); deliberate attacks and constraint probes use raw PostgREST
//! with the acting user's JWT. Never prints credentials, access tokens or full mapping tokens.
//!
//! Usage:
//!   validate_slice5 locals                    # show the two Helium profiles that will be used
//!   validate_slice5 run [creds-env-file]      # full validation + cleanup
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use synchrome_auth::{AuthClient, Session};
use synchrome_cloud::{
    auth_headers, CloudClient, CloudError, MappingStatus, ReqwestRest, TokenState,
};
use synchrome_config::{AppConfig, SupabaseConfig};
use synchrome_device::DeviceIdentity;
use synchrome_http::HttpClient;
use synchrome_types::BrowserKind;
use uuid::Uuid;

struct R(Vec<(String, bool)>);
impl R {
    fn check(&mut self, name: &str, ok: bool, detail: impl AsRef<str>) -> bool {
        println!(
            "  [{}] {name} — {}",
            if ok { "PASS" } else { "FAIL" },
            detail.as_ref()
        );
        self.0.push((name.into(), ok));
        ok
    }
}

/// Parse `KEY=value` lines, tolerating `export KEY=...`, surrounding quotes and CRLF.
fn creds(path: &str) -> [String; 4] {
    let text = std::fs::read_to_string(path).expect("creds file");
    let get = |k: &str| {
        text.lines()
            .map(|l| l.trim().trim_start_matches("export ").trim())
            .find_map(|l| l.strip_prefix(&format!("{k}=")).map(unquote))
            .unwrap_or_default()
    };
    [
        get("USER_A_EMAIL"),
        get("USER_A_PASSWORD"),
        get("USER_B_EMAIL"),
        get("USER_B_PASSWORD"),
    ]
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    for q in ['"', '\''] {
        if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
            return v[1..v.len() - 1].to_string();
        }
    }
    v.to_string()
}

async fn raw(
    cfg: &SupabaseConfig,
    http: &HttpClient,
    s: Option<&Session>,
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> (u16, String) {
    let mut owned: Vec<(String, String)> = match s {
        Some(s) => auth_headers(cfg, s),
        None => vec![("apikey".into(), cfg.anon_key.clone())],
    };
    owned.push(("Prefer".into(), "return=representation".into()));
    owned.push(("Content-Type".into(), "application/json".into()));
    let h: Vec<(&str, &str)> = owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let url = format!("{}/rest/v1{}", cfg.url.trim_end_matches('/'), path);
    match http.request_json(method, &url, &h, body.as_ref()).await {
        Ok((st, b)) => (st, String::from_utf8_lossy(&b).into_owned()),
        Err(e) => (0, e.to_string()),
    }
}

fn rows(body: &str) -> Vec<serde_json::Value> {
    serde_json::from_str(body).unwrap_or_default()
}
fn code(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["code"].as_str().map(String::from))
        .unwrap_or_default()
}

/// Two real Helium profiles detected by the app's own discovery (same rule the app enforces).
fn two_helium_profiles() -> Option<(PathBuf, String, String)> {
    let b = synchrome_browser::discover_browsers()
        .into_iter()
        .find(|b| b.kind == BrowserKind::Helium)?;
    let mut dirs: Vec<String> = b.profiles.iter().map(|p| p.dir_name.clone()).collect();
    dirs.sort();
    if dirs.len() < 2 {
        return None;
    }
    Some((
        PathBuf::from(b.user_data_dir),
        dirs[0].clone(),
        dirs[1].clone(),
    ))
}

/// Child entry names of the extension storage areas we must NOT touch in this slice.
fn storage_snapshot(udd: &Path, dirs: &[&str]) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    for d in dirs {
        for area in [
            "Local Extension Settings",
            "Sync Extension Settings",
            "IndexedDB",
        ] {
            let p = udd.join(d).join(area);
            let mut names: Vec<String> = std::fs::read_dir(&p)
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            names.sort();
            out.insert(format!("{d}/{area}"), names);
        }
    }
    out
}

async fn run(path: &str) -> bool {
    let cfg = AppConfig::load().expect(".env client config").supabase;
    let http = HttpClient::new().unwrap();
    let auth = AuthClient::new(cfg.clone()).unwrap();
    let cloud: CloudClient<ReqwestRest> = CloudClient::new(cfg.clone()).unwrap();
    let [a_email, a_pw, b_email, b_pw] = creds(path);
    let mut r = R(vec![]);
    let tag = &Uuid::new_v4().simple().to_string()[..6];

    let Some((udd, dir_x, dir_y)) = two_helium_profiles() else {
        r.check(
            "7 two real Helium profiles detected",
            false,
            "fewer than 2 Helium profiles",
        );
        return false;
    };
    let before_fs = storage_snapshot(&udd, &[&dir_x, &dir_y]);

    // 1. login A
    let sa = match auth.login_password(&a_email, &a_pw).await {
        Ok(s) => {
            let expected = std::env::var("EXPECTED_A_ID").ok();
            let ok = expected.as_deref().map_or(true, |e| e == s.user_id);
            r.check(
                "1 login A (user_id matches expected)",
                ok,
                format!("user_id {} expected {:?}", s.user_id, expected),
            );
            if !ok {
                std::process::exit(3);
            }
            s
        }
        Err(e) => {
            r.check("1 login A", false, e.to_string());
            // Distinct exit code: credentials problem, nothing created; caller keeps the file.
            std::process::exit(3);
        }
    };
    // 2. register the real local device
    let me = synchrome_device::load_or_create().expect("local identity");
    let dev = cloud.upsert_device(&sa, &me).await;
    r.check(
        "2 register real local device",
        matches!(&dev, Ok(d) if d.device_id == me.device_id),
        format!("device_id {}", me.device_id),
    );

    // 3-6. profiles
    let personal = cloud.create_profile(&sa, "Personal").await;
    let ok3 = r.check(
        "3 create cloud profile Personal",
        personal.is_ok(),
        format!("{:?}", personal.as_ref().map(|p| p.id)),
    );
    if !ok3 {
        return false;
    }
    let personal = personal.unwrap();
    let dup = cloud.create_profile(&sa, "Personal").await;
    r.check(
        "4 duplicate name -> Conflict",
        matches!(dup, Err(CloudError::Conflict(_))),
        format!("{dup:?}").chars().take(90).collect::<String>(),
    );
    let renamed_name = format!("Personal {tag}");
    let ren = cloud.rename_profile(&sa, personal.id, &renamed_name).await;
    r.check(
        "5 rename Personal",
        matches!(&ren, Ok(p) if p.name == renamed_name),
        format!("-> {renamed_name:?}"),
    );
    let aux = cloud
        .create_profile(&sa, &format!("Aux {tag}"))
        .await
        .expect("aux profile");
    let ren_dup = cloud.rename_profile(&sa, aux.id, &renamed_name).await;
    r.check(
        "6 rename aux to an existing name -> Conflict",
        matches!(ren_dup, Err(CloudError::Conflict(_))),
        "unique (account_id, name)",
    );

    // 7-9. two local Helium profiles, same cloud profile, same device
    r.check(
        "7 two real Helium profiles detected by the app",
        true,
        format!("{dir_x:?} and {dir_y:?}"),
    );
    let m1 = cloud
        .map_local_profile(&sa, personal.id, me.device_id, BrowserKind::Helium, &dir_x)
        .await;
    let m1_ok = matches!(&m1, Ok(m) if m.token_state == TokenState::PendingLocalWrite && m.mapping_token.is_well_formed());
    let summary_json = m1
        .as_ref()
        .map(|m| {
            serde_json::to_string(&m.summary(
                Some(renamed_name.clone()),
                Some(me.device_name.clone()),
                me.device_id,
            ))
            .unwrap()
        })
        .unwrap_or_default();
    let token_leak = m1
        .as_ref()
        .map(|m| {
            summary_json.contains(m.mapping_token.expose())
                || format!("{m:?}").contains(m.mapping_token.expose())
        })
        .unwrap_or(true);
    r.check(
        "8 map Personal -> Helium/X (pending, well-formed token, not in summary/Debug)",
        m1_ok && !token_leak && summary_json.contains("token_pending_local_write"),
        format!(
            "hint {}",
            m1.as_ref()
                .map(|m| m.mapping_token.hint())
                .unwrap_or_default()
        ),
    );
    let m2 = cloud
        .map_local_profile(&sa, personal.id, me.device_id, BrowserKind::Helium, &dir_y)
        .await;
    let same_pair = matches!((&m1, &m2), (Ok(a), Ok(b)) if a.profile_id == b.profile_id && a.device_id == b.device_id && a.id != b.id);
    r.check(
        "9 map the SAME Personal -> Helium/Y on the SAME device",
        same_pair,
        "two mappings, one (profile_id, device_id)",
    );
    let (Ok(m1), Ok(m2)) = (m1, m2) else {
        cleanup(
            &cfg,
            &http,
            &sa,
            None,
            &me,
            None,
            &[personal.id, aux.id],
            &[],
            &mut r,
        )
        .await;
        return false;
    };

    // 10. same local profile again (same cloud profile and another one)
    let again_same = cloud
        .map_local_profile(&sa, personal.id, me.device_id, BrowserKind::Helium, &dir_x)
        .await;
    let again_other = cloud
        .map_local_profile(&sa, aux.id, me.device_id, BrowserKind::Helium, &dir_x)
        .await;
    r.check(
        "10 remap Helium/X -> Conflict (to same and to another cloud profile)",
        matches!(again_same, Err(CloudError::Conflict(_)))
            && matches!(again_other, Err(CloudError::Conflict(_))),
        "unique (device, browser, dir)",
    );

    // 11. second synthetic device of A
    let syn = DeviceIdentity {
        device_id: Uuid::new_v4(),
        device_name: format!("slice5-synthetic-{tag}"),
        platform: me.platform.clone(),
        os_version: None,
        app_version: me.app_version.clone(),
    };
    cloud
        .upsert_device(&sa, &syn)
        .await
        .expect("synthetic device");
    let m3 = cloud
        .map_local_profile(
            &sa,
            personal.id,
            syn.device_id,
            BrowserKind::Helium,
            "Default",
        )
        .await;
    r.check(
        "11 map Personal from a second (synthetic) device",
        m3.is_ok(),
        format!("synthetic device {}", syn.device_id),
    );

    // 12. lookups
    let by_local = cloud
        .mapping_for_local_profile(&sa, me.device_id, BrowserKind::Helium, &dir_x)
        .await
        .unwrap();
    let by_cloud = cloud
        .mapping_for_cloud_profile(&sa, personal.id)
        .await
        .unwrap();
    let sums: Vec<_> = by_cloud
        .iter()
        .map(|m| m.summary(None, None, me.device_id))
        .collect();
    let this_dev = sums.iter().filter(|s| s.is_this_device).count();
    let other_dev = sums
        .iter()
        .filter(|s| !s.is_this_device && s.device_id == syn.device_id)
        .count();
    r.check(
        "12 lookups: local -> 1, cloud -> 3, is_this_device 2 true / 1 false",
        by_local.as_ref().map(|m| m.id) == Some(m1.id)
            && by_cloud.len() == 3
            && this_dev == 2
            && other_dev == 1,
        format!(
            "by_local={} by_cloud={} this_device={this_dev} synthetic={other_dev}",
            by_local.is_some() as u8,
            by_cloud.len()
        ),
    );

    // 13. delete while mapped
    let del = cloud.delete_profile(&sa, personal.id).await;
    r.check(
        "13 delete Personal while mapped -> ProfileInUse",
        del == Err(CloudError::ProfileInUse),
        format!("{del:?}"),
    );

    // 14. confirm one token
    let conf = cloud.confirm_mapping_token(&sa, m1.id).await;
    r.check("14 confirm mapping token", matches!(&conf, Ok(m) if m.token_state == TokenState::Confirmed && m.token_confirmed_at.is_some())
        && conf.as_ref().map(|m| m.summary(None, None, me.device_id).status == MappingStatus::TokenConfirmed).unwrap_or(false),
        format!("token_confirmed_at {:?}", conf.as_ref().ok().and_then(|m| m.token_confirmed_at.clone())));

    // 15. column grants: identity/token/account are immutable for clients
    let p = format!("/profile_mappings?id=eq.{}", m2.id);
    let (s_tok, b_tok) = raw(
        &cfg,
        &http,
        Some(&sa),
        "PATCH",
        &p,
        Some(serde_json::json!({"mapping_token": "f".repeat(64)})),
    )
    .await;
    let (s_dev, b_dev) = raw(
        &cfg,
        &http,
        Some(&sa),
        "PATCH",
        &p,
        Some(serde_json::json!({"device_id": syn.device_id})),
    )
    .await;
    let (s_acc, b_acc) = raw(
        &cfg,
        &http,
        Some(&sa),
        "PATCH",
        &p,
        Some(serde_json::json!({"account_id": Uuid::new_v4()})),
    )
    .await;
    let unchanged = cloud
        .mapping_for_local_profile(&sa, me.device_id, BrowserKind::Helium, &dir_y)
        .await
        .unwrap()
        .map(|m| m.mapping_token == m2.mapping_token && m.device_id == m2.device_id)
        .unwrap_or(false);
    let denied = |s: u16| s == 401 || s == 403;
    r.check(
        "15 PATCH mapping_token / device_id / account_id rejected (column grants)",
        denied(s_tok) && denied(s_dev) && denied(s_acc) && unchanged,
        format!(
            "token→{s_tok} {} | device→{s_dev} {} | account→{s_acc} {} | row unchanged={unchanged}",
            code(&b_tok),
            code(&b_dev),
            code(&b_acc)
        ),
    );

    // 16. user B
    let sb = match auth.login_password(&b_email, &b_pw).await {
        Ok(s) => {
            let expected = std::env::var("EXPECTED_B_ID").ok();
            let ok =
                expected.as_deref().map_or(true, |e| e == s.user_id) && s.user_id != sa.user_id;
            r.check(
                "16 login B (user_id matches expected, differs from A)",
                ok,
                format!("user_id {} expected {:?}", s.user_id, expected),
            );
            s
        }
        Err(e) => {
            r.check("16 login B", false, e.to_string());
            cleanup(
                &cfg,
                &http,
                &sa,
                None,
                &me,
                Some(&syn),
                &[personal.id, aux.id],
                &[m1.id, m2.id],
                &mut r,
            )
            .await;
            // A's test rows are already cleaned; credentials problem -> keep the file.
            std::process::exit(3);
        }
    };
    let syn_b = DeviceIdentity {
        device_id: Uuid::new_v4(),
        device_name: format!("slice5-B-{tag}"),
        ..syn.clone()
    };
    cloud.upsert_device(&sb, &syn_b).await.expect("B device");
    let b_prof = cloud
        .create_profile(&sb, &format!("B-profile {tag}"))
        .await
        .expect("B profile");
    let b_sees = cloud.list_mappings(&sb).await.unwrap();
    let b_unmap = cloud.unmap_local_profile(&sb, m2.id).await;
    let b_conf = cloud.confirm_mapping_token(&sb, m2.id).await;
    let a_prof_b_dev = cloud
        .map_local_profile(
            &sb,
            personal.id,
            syn_b.device_id,
            BrowserKind::Helium,
            "Default",
        )
        .await;
    let b_prof_a_dev = cloud
        .map_local_profile(
            &sb,
            b_prof.id,
            me.device_id,
            BrowserKind::Helium,
            "Profile 99",
        )
        .await;
    let tok = synchrome_cloud::MappingToken::generate().unwrap();
    let (s_ins, b_ins) = raw(&cfg, &http, Some(&sb), "POST", "/profile_mappings", Some(serde_json::json!({
        "account_id": sa.user_id, "profile_id": personal.id, "device_id": me.device_id,
        "browser_kind": "helium", "browser_profile_dir": "Profile 98", "mapping_token": tok.expose() }))).await;
    let a_still = cloud.list_mappings(&sa).await.unwrap().len();
    r.check(
        "16a B sees 0 of A's mappings",
        b_sees.is_empty(),
        format!("B sees {}", b_sees.len()),
    );
    r.check(
        "16b B cannot unmap A's mapping",
        b_unmap == Err(CloudError::NotFound),
        format!("{b_unmap:?}"),
    );
    r.check(
        "16c B cannot confirm A's mapping",
        matches!(b_conf, Err(CloudError::NotFound)),
        format!("{:?}", b_conf.err()),
    );
    r.check(
        "16d A's profile + B's device -> UnknownReference",
        matches!(a_prof_b_dev, Err(CloudError::UnknownReference(_))),
        "composite FK (account_id, profile_id)",
    );
    r.check(
        "16e B's profile + A's device -> UnknownReference",
        matches!(b_prof_a_dev, Err(CloudError::UnknownReference(_))),
        "composite FK (account_id, device_id)",
    );
    r.check(
        "16f insert with A's account_id explicitly -> rejected",
        s_ins >= 400 && a_still == 3,
        format!(
            "HTTP {s_ins} {} ; A still has {a_still} mappings",
            code(&b_ins)
        ),
    );

    // 17. direct constraint probes (A's own valid profile/device, one bad field each)
    let base = |dir: &str, kind: &str, token: &str| {
        serde_json::json!({
        "profile_id": aux.id, "device_id": me.device_id, "browser_kind": kind, "browser_profile_dir": dir, "mapping_token": token })
    };
    let good = synchrome_cloud::MappingToken::generate().unwrap();
    let (s_dir, b_dir) = raw(
        &cfg,
        &http,
        Some(&sa),
        "POST",
        "/profile_mappings",
        Some(base("../evil", "helium", good.expose())),
    )
    .await;
    let (s_t, b_t) = raw(
        &cfg,
        &http,
        Some(&sa),
        "POST",
        "/profile_mappings",
        Some(base("Probe 1", "helium", "NOT-A-TOKEN")),
    )
    .await;
    let (s_k, b_k) = raw(
        &cfg,
        &http,
        Some(&sa),
        "POST",
        "/profile_mappings",
        Some(base("Probe 2", "firefox", good.expose())),
    )
    .await;
    let (s_anon, _) = raw(
        &cfg,
        &http,
        None,
        "GET",
        "/profile_mappings?select=id",
        None,
    )
    .await;
    r.check(
        "17a invalid browser_profile_dir rejected",
        s_dir >= 400 && code(&b_dir) == "23514",
        format!("HTTP {s_dir} {}", code(&b_dir)),
    );
    r.check(
        "17b invalid mapping_token rejected",
        s_t >= 400 && code(&b_t) == "23514",
        format!("HTTP {s_t} {}", code(&b_t)),
    );
    r.check(
        "17c invalid browser_kind rejected",
        s_k >= 400 && code(&b_k) == "23514",
        format!("HTTP {s_k} {}", code(&b_k)),
    );
    r.check(
        "17d anon without JWT -> 401",
        s_anon == 401,
        format!("HTTP {s_anon}"),
    );

    // 18. CASCADE: delete the synthetic device
    let (s_dd, b_dd) = raw(
        &cfg,
        &http,
        Some(&sa),
        "DELETE",
        &format!("/devices?device_id=eq.{}", syn.device_id),
        None,
    )
    .await;
    let after = cloud
        .mapping_for_cloud_profile(&sa, personal.id)
        .await
        .unwrap();
    let only_real = after.iter().all(|m| m.device_id == me.device_id);
    r.check(
        "18 CASCADE: deleting the synthetic device removes its mapping only",
        s_dd == 200 && rows(&b_dd).len() == 1 && after.len() == 2 && only_real,
        format!(
            "Personal mappings now {} (all on real device: {only_real})",
            after.len()
        ),
    );

    // 19. RESTRICT still
    let del2 = cloud.delete_profile(&sa, personal.id).await;
    r.check(
        "19 RESTRICT: delete Personal with 2 local mappings -> ProfileInUse",
        del2 == Err(CloudError::ProfileInUse),
        format!("{del2:?}"),
    );

    // 20. unmap both, then delete works
    let u1 = cloud.unmap_local_profile(&sa, m1.id).await;
    let u2 = cloud.unmap_local_profile(&sa, m2.id).await;
    let del3 = cloud.delete_profile(&sa, personal.id).await;
    r.check(
        "20 unmap both locals, then delete Personal succeeds",
        u1.is_ok() && u2.is_ok() && del3.is_ok(),
        format!("{u1:?} {u2:?} {del3:?}"),
    );

    // 21. nothing written locally
    let after_fs = storage_snapshot(&udd, &[&dir_x, &dir_y]);
    let probe_dir_present = after_fs
        .values()
        .flatten()
        .any(|n| n.contains("bahaodcchlkcffhkjkemenhdomfnjekm"));
    r.check(
        "21 no local write: extension storage entries unchanged in both Helium profiles",
        before_fs == after_fs && !probe_dir_present,
        format!(
            "{} storage areas compared, identical entry sets",
            before_fs.len()
        ),
    );

    // 22. cleanup
    cleanup(
        &cfg,
        &http,
        &sa,
        Some(&sb),
        &me,
        None,
        &[aux.id],
        &[],
        &mut r,
    )
    .await;
    let _ = raw(
        &cfg,
        &http,
        Some(&sb),
        "DELETE",
        &format!("/profiles?id=eq.{}", b_prof.id),
        None,
    )
    .await;
    let _ = raw(
        &cfg,
        &http,
        Some(&sb),
        "DELETE",
        &format!("/devices?device_id=eq.{}", syn_b.device_id),
        None,
    )
    .await;
    let mut left = vec![];
    for (who, s) in [("A", &sa), ("B", &sb)] {
        for t in ["profiles", "devices", "profile_mappings"] {
            let col = if t == "devices" { "device_id" } else { "id" };
            let n = rows(
                &raw(
                    &cfg,
                    &http,
                    Some(s),
                    "GET",
                    &format!("/{t}?select={col}"),
                    None,
                )
                .await
                .1,
            )
            .len();
            left.push(format!("{who}.{t}={n}"));
        }
    }
    r.check(
        "22 cleanup: both accounts back to 0 profiles / devices / mappings",
        left.iter().all(|x| x.ends_with("=0")),
        left.join(" "),
    );

    let failed: Vec<_> =
        r.0.iter()
            .filter(|(_, ok)| !ok)
            .map(|(n, _)| n.clone())
            .collect();
    println!(
        "\n=== slice 5 summary: {} pass, {} fail ===",
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
    _sb: Option<&Session>,
    me: &DeviceIdentity,
    syn: Option<&DeviceIdentity>,
    profiles: &[Uuid],
    mappings: &[Uuid],
    _r: &mut R,
) {
    for m in mappings {
        let _ = raw(
            cfg,
            http,
            Some(sa),
            "DELETE",
            &format!("/profile_mappings?id=eq.{m}"),
            None,
        )
        .await;
    }
    let _ = raw(
        cfg,
        http,
        Some(sa),
        "DELETE",
        "/profile_mappings?id=not.is.null",
        None,
    )
    .await;
    if let Some(d) = syn {
        let _ = raw(
            cfg,
            http,
            Some(sa),
            "DELETE",
            &format!("/devices?device_id=eq.{}", d.device_id),
            None,
        )
        .await;
    }
    for p in profiles {
        let _ = raw(
            cfg,
            http,
            Some(sa),
            "DELETE",
            &format!("/profiles?id=eq.{p}"),
            None,
        )
        .await;
    }
    let _ = raw(
        cfg,
        http,
        Some(sa),
        "DELETE",
        &format!("/devices?device_id=eq.{}", me.device_id),
        None,
    )
    .await;
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str).unwrap_or("run") {
        "locals" => match two_helium_profiles() {
            Some((udd, x, y)) => println!(
                "Helium user-data-dir: {}\n  profile X: {x:?}\n  profile Y: {y:?}",
                udd.display()
            ),
            None => {
                println!("fewer than 2 Helium profiles detected");
                std::process::exit(1)
            }
        },
        "run" => {
            let path = args
                .get(2)
                .cloned()
                .unwrap_or_else(|| "/tmp/synchrome-slice5-users.env".into());
            let ok = tokio::runtime::Runtime::new().unwrap().block_on(run(&path));
            std::process::exit(if ok { 0 } else { 1 });
        }
        o => {
            eprintln!("unknown command {o}");
            std::process::exit(2)
        }
    }
}
