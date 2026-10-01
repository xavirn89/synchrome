// E1 TCC harness — NON-DESTRUCTIVE probe of Chromium profile access on macOS.
// Reads/stats paths and tests writes ONLY inside directories passed as disposable.
// Never writes to a real profile or the real External Extensions dir.
use std::fs;
use std::io::Read;
use std::path::Path;

fn home() -> String { std::env::var("HOME").unwrap_or_default() }

fn errno_str(e: &std::io::Error) -> String {
    match e.raw_os_error() {
        Some(1) => "EPERM(1 operation not permitted)".into(),
        Some(2) => "ENOENT(2 no such file)".into(),
        Some(13) => "EACCES(13 permission denied)".into(),
        Some(n) => format!("errno {}", n),
        None => format!("{}", e),
    }
}

// Try to read the first byte of a file. Returns Ok(true) if readable.
fn probe_read(p: &str) -> String {
    match fs::File::open(p) {
        Ok(mut f) => { let mut b = [0u8;1]; match f.read(&mut b) { Ok(_) => "READ ok".into(), Err(e) => format!("open ok, read {}", errno_str(&e)) } }
        Err(e) => errno_str(&e),
    }
}

// List a directory (read access to a dir).
fn probe_list(p: &str) -> String {
    match fs::read_dir(p) {
        Ok(rd) => format!("LIST ok ({} entries)", rd.count()),
        Err(e) => errno_str(&e),
    }
}

// Stat only (metadata) — the least invasive check.
fn probe_stat(p: &str) -> String {
    match fs::symlink_metadata(p) {
        Ok(m) => format!("STAT ok ({})", if m.is_dir() {"dir"} else {"file"}),
        Err(e) => errno_str(&e),
    }
}

// Write test: create + remove a temp file INSIDE dir. Only call on disposable dirs.
fn probe_write(dir: &str) -> String {
    let p = format!("{}/.e1_write_test_{}", dir, std::process::id());
    match fs::write(&p, b"e1") {
        Ok(_) => { let _ = fs::remove_file(&p); "WRITE ok".into() }
        Err(e) => errno_str(&e),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("probe");
    let h = home();
    println!("# E1 harness  pid={}  exe={}", std::process::id(),
             std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default());
    println!("# responsible-process hint: argv0={}", args.get(0).cloned().unwrap_or_default());

    // (browser, label, path, kind)  kind: read|list|stat
    let chrome = format!("{}/Library/Application Support/Google/Chrome", h);
    let brave  = format!("{}/Library/Application Support/BraveSoftware/Brave-Browser", h);
    let edge   = format!("{}/Library/Application Support/Microsoft Edge", h);
    let helium = format!("{}/Library/Application Support/net.imput.helium", h);

    let probes: Vec<(&str,&str,String,&str)> = vec![
        // TCC-protected canaries (expect EPERM without Full Disk Access)
        ("(canary)","TCC.db", format!("{}/Library/Application Support/com.apple.TCC/TCC.db", h), "read"),
        // Helium — not a well-known TCC-guarded bundle
        ("Helium","Local State", format!("{}/Local State", helium), "read"),
        ("Helium","profile dir", format!("{}/Profile 2", helium), "list"),
        ("Helium","Preferences", format!("{}/Profile 2/Preferences", helium), "read"),
        ("Helium","History", format!("{}/Profile 2/History", helium), "read"),
        ("Helium","Local Ext Settings", format!("{}/Profile 2/Local Extension Settings", helium), "list"),
        ("Helium","NativeMessagingHosts", format!("{}/NativeMessagingHosts", helium), "list"),
        // Google Chrome — TCC-protected
        ("Chrome","root", chrome.clone(), "list"),
        ("Chrome","Local State", format!("{}/Local State", chrome), "read"),
        ("Chrome","Default/Preferences", format!("{}/Default/Preferences", chrome), "read"),
        ("Chrome","Default/History", format!("{}/Default/History", chrome), "read"),
        ("Chrome","Default/Local Ext Settings", format!("{}/Default/Local Extension Settings", chrome), "list"),
        ("Chrome","NativeMessagingHosts", format!("{}/NativeMessagingHosts", chrome), "list"),
        ("Chrome","External Extensions (canonical)", format!("{}/External Extensions", chrome), "stat"),
        ("Chrome","External Extensions LIST", format!("{}/External Extensions", chrome), "list"),
        // Brave
        ("Brave","root", brave.clone(), "list"),
        ("Brave","Default/History", format!("{}/Default/History", brave), "read"),
        // Edge (may be absent)
        ("Edge","root", edge.clone(), "list"),
    ];

    let mut out = String::new();
    out.push_str(&format!("# exe={}\n", std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default()));
    out.push_str(&format!("{:<8} {:<34} {:<10} {}\n", "BROWSER","PATH","KIND","RESULT"));
    for (b,l,p,k) in &probes {
        let r = match *k { "read"=>probe_read(p), "list"=>probe_list(p), _=>probe_stat(p) };
        out.push_str(&format!("{:<8} {:<34} {:<10} {}\n", b, l, k, r));
    }
    print!("\n{}", out);
    let outpath = std::env::var("E1_OUT").unwrap_or_else(|_| "/tmp/e1probe_out.txt".into());
    let _ = std::fs::write(&outpath, &out);

    // Disposable write test (only when a dir is passed and clearly temp)
    if mode == "writetest" {
        if let Some(dir) = args.get(2) {
            println!("\nWRITE test in disposable dir {}: {}", dir, probe_write(dir));
        }
    }
}
