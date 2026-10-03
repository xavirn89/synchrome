"""Clean-install smoke of the RELEASE app (v1 closure) — driven through macOS accessibility.

Installs `target/release/bundle/dmg/Synchrome_*.dmg` into /Applications (replacing the previous
build: also an update test for the Full Disk Access grant), starts it on an EMPTY app data
folder and an empty Keychain entry, and walks the user path in the real window: sign in, a
disposable Helium profile (declared as an extra data folder) → Create Synchrome profile →
Install connector (manifest must point into /Applications) → companion loads and confirms →
first sync → "Connected"; quit + relaunch; logout + login (one Realtime connection); the
disposable Chrome folder inside Chrome's protected directory shows Accessible (FDA kept by the
update). Prints labels/counts only; the window's other content (real browsers) is not printed.

  python3 e2e_release_smoke.py <creds-env-file>
"""
import glob, json, os, shutil, subprocess, sys, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import e2e_handshake as h  # noqa: E402
import e2e_bookmarks as eb  # noqa: E402
import relax as rx  # noqa: E402

check = h.check
ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
SMOKE = "/tmp/synchrome-e2e/smoke-helium"
NESTED = os.path.expanduser("~/Library/Application Support/Google/Chrome/Synchrome-E2E-disposable")
NESTED_NAME = "Google Chrome — E2E disposable (nested)"


def lines():
    return [x for _, x in rx.texts()]


def block(title, n=24):
    ls = lines()
    i = next((k for k, x in enumerate(ls) if x == title), -1)
    return ls[i:i + n] if i >= 0 else []


def steps_badge(ls):
    i = next((k for k, x in enumerate(ls) if x == "Getting started"), -1)
    return next((x for x in ls[i:i + 4] if x == "Connected" or (x.count("/") == 1 and len(x) < 8)), None) if i >= 0 else None


def wait_rt(pred, timeout=40):
    end = time.time() + timeout
    while time.time() < end:
        r = rx.rt()
        if pred(r):
            return r
        time.sleep(1)
    return rx.rt()


def sign_in(creds):
    email, pw = rx.creds(creds, "A")
    a = rx.type_field("Email", email)
    b = rx.type_field("Password", pw)
    del email, pw
    rx.click("Sign in")
    return a == b == "typed" and rx.wait_text(lambda t: "Signed in as" in t, 40) is not None


def main(creds):
    print("=== release clean-install smoke ===")
    dmg = sorted(glob.glob(os.path.join(ROOT, "target/release/bundle/dmg/Synchrome_*.dmg")))[-1]
    if rx.running():
        rx.quit_app()
    check("0 no Synchrome app data, no Keychain session (clean machine state for the app)",
          not os.path.exists(rx.APP_DATA) and not rx.keychain_has_session(), "")
    out = subprocess.run(["hdiutil", "attach", "-nobrowse", "-readonly", dmg], capture_output=True, text=True).stdout
    mnt = [l.split("\t")[-1] for l in out.splitlines() if "/Volumes/" in l][0]
    shutil.rmtree("/Applications/Synchrome.app", ignore_errors=True)
    subprocess.run(["cp", "-R", os.path.join(mnt, "Synchrome.app"), "/Applications/"], check=True)
    subprocess.run(["hdiutil", "detach", mnt, "-quiet"])
    v = subprocess.run(["codesign", "--verify", "--deep", "--strict", "/Applications/Synchrome.app"], capture_output=True)
    sig = subprocess.run(["codesign", "-dv", "/Applications/Synchrome.app"], capture_output=True, text=True).stderr
    check("1 installed from the DMG into /Applications; signature valid (Developer ID, Team YV44FR3LG3, com.synchrome.app)",
          v.returncode == 0 and "TeamIdentifier=YV44FR3LG3" in sig and "Identifier=com.synchrome.app" in sig, os.path.basename(dmg))

    # first run
    shutil.rmtree(SMOKE, ignore_errors=True)
    os.makedirs(SMOKE)
    hl = eb.Helium(SMOKE)  # a fresh disposable Helium profile with the companion loaded (unlinked)
    hl.stop()
    rx.launch()
    rx.wait_text(lambda t: "Getting started" in t and "Synchrome Cloud is reachable" in t and "Sign in" in t, 60)
    time.sleep(2)
    ls = lines()
    check("2 first run: window up, signed out, Getting started shows the steps, Synchrome Cloud Connected, installed in Applications",
          "Getting started" in ls and "Signed out" in ls and "Synchrome is installed in Applications" in ls and "Connected" in ls
          and "Synchrome Cloud is reachable" in ls, json.dumps({"badge": steps_badge(ls)}))
    check("2b updates are manual (stated in the app), no updater configured", any(x.startswith("Updates: Synchrome 1.0.0 is updated manually") for x in ls), "")
    check("3 sign in through the real login form (exact account)", sign_in(creds), "")
    r = wait_rt(lambda r: r.get("connected"))
    check("3b Keychain session stored; one Realtime connection", rx.keychain_has_session() and r.get("connects") == 1, json.dumps({k: r.get(k) for k in ("connected", "connects")}))

    # data folders: the disposable Helium + the disposable Chrome folder inside Chrome's protected dir
    json.dump([{"kind": "helium", "user_data_dir": SMOKE, "label": "Smoke"},
               {"kind": "chrome", "user_data_dir": NESTED, "label": "E2E disposable (nested)"}],
              open(os.path.join(rx.APP_DATA, "browsers.json"), "w"))
    rx.click("Refresh")
    rx.wait_text(lambda t: "Helium — Smoke" in t, 40)
    nb = block(NESTED_NAME, 8)
    check("4 Full Disk Access survived the update to the final build: the disposable Chrome folder is Accessible with its profile",
          "Accessible" in nb and "Default" in block(NESTED_NAME, 14), json.dumps([x for x in nb if x in ("Accessible", "Requires Full Disk Access")]))
    # buttons are clicked strictly INSIDE the disposable "Helium — Smoke" entry (the first one
    # after its heading), never on a real profile
    c1 = rx.click_after("Helium — Smoke", "Create Synchrome profile")
    blob = rx.wait_text(lambda t: "Token written" in t or "Extension confirmed" in t, 60)
    reg = json.load(open(os.path.join(rx.APP_DATA, "local_tokens.json")))
    regs = reg.get("mappings") if isinstance(reg, dict) else reg
    check("5 Create Synchrome profile on the disposable Helium profile: cloud profile + mapping, token written (browser closed); ONLY that folder linked",
          c1 == "clicked" and blob is not None and [m["user_data_dir"] for m in regs] == [SMOKE], json.dumps({"linked_folders": len(regs)}))
    rx.click_after("Helium — Smoke", "Install connector")
    rx.wait_text(lambda t: "Connector installed" in t or "Connector install failed" in t, 30)
    man = os.path.join(SMOKE, "NativeMessagingHosts", "com.synchrome.host.json")
    path = json.load(open(man)).get("path") if os.path.exists(man) else None
    check("6 Install connector: native messaging manifest points at the app in /Applications (stable path, not Documents/Desktop/Downloads)",
          path == "/Applications/Synchrome.app/Contents/MacOS/synchrome-desktop", path or "missing")
    check("7 the app shows where the bundled companion extension is (Contents/Resources/extension)",
          "/Applications/Synchrome.app/Contents/Resources/extension" in lines(), "")

    hl = eb.Helium(SMOKE)
    st = hl.wait(lambda s: s.get("status") == "connected" and s.get("token_state") == "confirmed", 60) or {}
    check("8 the companion connects through the release host and confirms the mapping token",
          st.get("status") == "connected" and st.get("token_state") == "confirmed", json.dumps({"status": st.get("status"), "token": st.get("token_state")}))
    hl.js("""(async () => { const bar = (await chrome.bookmarks.getTree())[0].children.find(c => c.folderType === 'bookmarks-bar' || c.id === '1');
      await chrome.bookmarks.create({parentId: bar.id, title: 'Smoke bookmark', url: 'https://example.com/smoke'}); return true; })()""")
    time.sleep(8)
    rx.click("Refresh")
    blob = rx.wait_text(lambda t: "Extension confirmed" in t and "Up to date" in t, 60)
    ls = lines()
    sm = block("Helium — Smoke", 30)
    check("9 first sync: bookmarks Up to date (1 bookmark), Getting started badge Connected",
          blob is not None and "1 bookmarks · 0 folders" in sm and steps_badge(ls) == "Connected", json.dumps({"badge": steps_badge(ls)}))
    hl.stop()

    # restart
    rx.quit_app()
    rx.launch()
    r = wait_rt(lambda r: r.get("connected") and r.get("pulls", 0) >= 1)
    rx.wait_text(lambda t: "Signed in" in t and "Extension confirmed" in t, 60)
    ls = lines()
    check("10 quit + relaunch: still signed in, mapping confirmed, Realtime reconnected (one connection) with a catch-up pull",
          "Signed in" in ls and "Extension confirmed" in ls and r.get("connects") == 1 and r.get("pulls", 0) >= 1, json.dumps({k: r.get(k) for k in ("connects", "pulls")}))

    # logout / login
    rx.click("Sign out")
    rx.wait_text(lambda t: "Signed out" in t, 30)
    r = wait_rt(lambda r: r.get("account_set") is False and r.get("connected") is False, 20)
    check("11 logout: Keychain session removed, Realtime closed", not rx.keychain_has_session() and r.get("connected") is False, "")
    check("12 login again", sign_in(creds), "")
    r = wait_rt(lambda r: r.get("connected") and r.get("connects") == 2)
    rx.wait_text(lambda t: "Extension confirmed" in t, 60)
    ls = lines()
    check("12b exactly one new Realtime connection; mapping still confirmed", r.get("connects") == 2 and "Extension confirmed" in ls, json.dumps({k: r.get(k) for k in ("connects", "reconnects")}))
    rx.quit_app()


if __name__ == "__main__":
    try:
        main(sys.argv[1])
    except Exception as ex:  # noqa: BLE001
        import traceback
        traceback.print_exc()
        check("harness completed without exception", False, type(ex).__name__ + ": " + str(ex)[:200])
    failed = [n for n, ok in h.results if not ok]
    print(f"\n=== release smoke: {len(h.results) - len(failed)} pass, {len(failed)} fail ===")
    if failed:
        print("  failures: " + ", ".join(failed))
    sys.exit(1 if failed else 0)
