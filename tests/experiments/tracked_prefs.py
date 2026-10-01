import json, os, shutil, signal, subprocess, sys, time
bin_, name = sys.argv[1], sys.argv[2]
base = os.path.dirname(os.path.abspath(__file__)); udd = os.path.join(base, f"udd-{name}")
shutil.rmtree(udd, ignore_errors=True)
def run(secs):
    p = subprocess.Popen([bin_, f"--user-data-dir={udd}", "--headless=new", "--no-first-run", "--no-default-browser-check", "--disable-gpu", "about:blank"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(secs); p.send_signal(signal.SIGTERM)
    try: p.wait(20)
    except subprocess.TimeoutExpired: p.kill(); print("  (killed)")
prof = os.path.join(udd, "Default")
def load(f):
    p = os.path.join(prof, f); return json.load(open(p)) if os.path.exists(p) else None
def get(d, path):
    for k in path.split("."):
        if not isinstance(d, dict) or k not in d: return "<absent>"
        d = d[k]
    return d
print(f"=== {name} ===")
run(15)
sp, pr = load("Secure Preferences"), load("Preferences")
print("Secure Preferences exists:", sp is not None, "| macs paths:", len(get(sp,"protection.macs")) if sp and isinstance(get(sp,"protection.macs"),dict) else 0, "| super_mac:", bool(sp and get(sp,"protection.super_mac")!="<absent>"))
print("initial homepage SP:", get(sp,"homepage"), "PR:", get(pr,"homepage"), "| show_home_button SP:", get(sp,"browser.show_home_button"), "PR:", get(pr,"browser.show_home_button"))
# Tamper: tracked prefs written WITHOUT updating MACs
sp["homepage"] = "https://example.com/synchrome-tracked-test"; sp["homepage_is_newtabpage"] = False
sp.setdefault("browser", {})["show_home_button"] = True
sp.setdefault("session", {})["restore_on_startup"] = 4; sp["session"]["startup_urls"] = ["https://example.org/startup"]
json.dump(sp, open(os.path.join(prof,"Secure Preferences"),"w"))
# Untracked pref written in Preferences
pr.setdefault("bookmark_bar", {})["show_on_all_tabs"] = True
pr.setdefault("intl", {})["accept_languages"] = "fr,en-US,en"
json.dump(pr, open(os.path.join(prof,"Preferences"),"w"))
run(15)
sp2, pr2 = load("Secure Preferences"), load("Preferences")
print("AFTER relaunch:")
for k in ["homepage","homepage_is_newtabpage","browser.show_home_button","session.restore_on_startup","session.startup_urls"]:
    print(f"  tracked {k}: SP={get(sp2,k)} PR={get(pr2,k)}")
print("  untracked bookmark_bar.show_on_all_tabs:", get(pr2,"bookmark_bar.show_on_all_tabs"), "| intl.accept_languages:", get(pr2,"intl.accept_languages"))
print("  reset markers: prefs.preference_reset_time:", get(sp2,"prefs.preference_reset_time"), "| tracked_preferences_reset:", get(sp2,"prefs.tracked_preferences_reset"))
print("  Local State keys:", sorted(json.load(open(os.path.join(udd,"Local State"))).keys())[:12])
