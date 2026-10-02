"""E13d — the product's Preferences writer against a real (DISPOSABLE) Helium profile, no cloud.

Write the four allowlisted logical values with `--dev-write-prefs` (browser closed), relaunch
Helium, read the effective values through chrome.settingsPrivate, check Secure Preferences was
not touched by the write and that Helium did not reset anything; then flip the bookmark bar
back to false after Helium re-added its visibility_state, and check again.

  python3 e13d_writer_on_helium.py
"""
import json, os, shutil, subprocess, sys, tempfile, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import e13_preferences as e  # noqa: E402
from cdp import CDP  # noqa: E402

APP = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "target", "debug", "synchrome-desktop")
KEYS = ["bookmark_bar.show_on_all_tabs", "download.prompt_for_download", "browser.enable_spellchecking", "intl.selected_languages", "intl.accept_languages"]


def dev(data, *args):
    p = subprocess.run([APP, *args], capture_output=True, text=True, env={**os.environ, "SYNCHROME_DATA_DIR": data}, timeout=60)
    return json.loads((p.stdout.strip().splitlines() or ["{}"])[-1])


def effective(udd):
    p = e.launch(e.BINS["helium"], udd)
    c = CDP(udd)
    s = e.settings_session(c)
    time.sleep(1)
    v = e.ev(c, s, "Promise.all(%s.map(k => new Promise(r => chrome.settingsPrivate.getPref(k, p => r(p ? p.value : null)))))" % json.dumps(KEYS))
    e.quit_(p, c)
    return dict(zip(KEYS, v or []))


def main():
    udd = tempfile.mkdtemp(prefix="synchrome-e13-helium-")
    data = tempfile.mkdtemp(prefix="synchrome-e13-data-")
    ok = True
    try:
        print("fresh:", effective(udd))
        before = dev(data, "--dev-read-prefs", udd, "Default", "helium")
        w = dev(data, "--dev-write-prefs", udd, "Default", "helium", json.dumps({
            "bookmark_bar.show_on_all_tabs": True, "download.prompt_for_download": True,
            "browser.enable_spellchecking": False, "intl.selected_languages": "es,en"}))
        after = dev(data, "--dev-read-prefs", udd, "Default", "helium")
        print("write:", w)
        print("rest unchanged by the write:", before["rest_sha"] == after["rest_sha"], "| Secure Preferences untouched:", before["secure_sha"] == after["secure_sha"])
        ok &= before["rest_sha"] == after["rest_sha"] and before["secure_sha"] == after["secure_sha"] and w.get("changed") == 4
        eff = effective(udd)
        print("effective after relaunch:", eff)
        ok &= eff["bookmark_bar.show_on_all_tabs"] is True and eff["download.prompt_for_download"] is True and eff["browser.enable_spellchecking"] is False and eff["intl.selected_languages"] == "es,en" and eff["intl.accept_languages"] == "es,en"
        r = dev(data, "--dev-read-prefs", udd, "Default", "helium")
        print("file after Helium ran:", r["values"], "raw:", r["raw"], "skipped:", r["skipped"])
        # Helium re-added visibility_state 0; Synchrome's false must still win
        w2 = dev(data, "--dev-write-prefs", udd, "Default", "helium", json.dumps({"bookmark_bar.show_on_all_tabs": False}))
        print("write false:", w2, "raw now:", dev(data, "--dev-read-prefs", udd, "Default", "helium")["raw"])
        eff2 = effective(udd)
        print("effective after relaunch:", eff2)
        ok &= eff2["bookmark_bar.show_on_all_tabs"] is False
        # reset to default (key removed)
        dev(data, "--dev-write-prefs", udd, "Default", "helium", json.dumps({"download.prompt_for_download": None}))
        eff3 = effective(udd)
        ok &= eff3["download.prompt_for_download"] is False
        print("after removing prompt_for_download:", eff3["download.prompt_for_download"])
        print("backups taken:", len(os.listdir(os.path.join(data, "backups", "browser-preferences"))))
        print("RESULT:", "OK" if ok else "FAIL")
    finally:
        for p_ in e.PROCS:
            if p_ and p_.poll() is None:
                p_.kill()
                p_.wait(10)
        shutil.rmtree(udd, ignore_errors=True)
        shutil.rmtree(data, ignore_errors=True)


if __name__ == "__main__":
    main()
