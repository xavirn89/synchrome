"""E3 — Install a Web Store extension via an External Extensions descriptor.

Writes <UserData>/External Extensions/<id>.json = {"external_update_url": "<cws update url>"}
then launches the browser and checks whether the extension dir appears under the profile.
Headless install of external extensions may be suppressed; if so, re-run without --headless.
Usage: python3 e3_external_extension.py "<browser binary>" <label> <extension-id>
"""
import json, os, shutil, signal, subprocess, sys, time

bin_, name = sys.argv[1], sys.argv[2]
ext_id = sys.argv[3] if len(sys.argv) > 3 else "cjpalhdlnbpafiamejdnhcphjbkeiagm"  # uBlock Origin
base = os.path.dirname(os.path.abspath(__file__))
udd = os.path.join(base, f"udd-e3-{name}")
shutil.rmtree(udd, ignore_errors=True)
os.makedirs(os.path.join(udd, "External Extensions"), exist_ok=True)
json.dump({"external_update_url": "https://clients2.google.com/service/update2/crx"},
          open(os.path.join(udd, "External Extensions", f"{ext_id}.json"), "w"))


def run(secs, headless=True):
    args = [bin_, f"--user-data-dir={udd}", "--no-first-run", "--no-default-browser-check",
            "--disable-gpu", "--profile-directory=Default"]
    if headless:
        args.append("--headless=new")
    args.append("about:blank")
    p = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(secs)
    p.send_signal(signal.SIGTERM)
    p.wait(25)


print(f"=== E3 {name} ext={ext_id} ===")
run(25)
ext_dir = os.path.join(udd, "Default", "Extensions", ext_id)
sp = os.path.join(udd, "Default", "Secure Preferences")
installed = os.path.isdir(ext_dir)
print("extension payload dir present:", installed)
if os.path.exists(sp):
    settings = json.load(open(sp)).get("extensions", {}).get("settings", {})
    e = settings.get(ext_id)
    if e:
        print("in Secure Preferences: location =", e.get("location"), "state =", e.get("state"),
              "from_webstore =", e.get("from_webstore"))
    else:
        print("not in Secure Preferences extensions.settings")
print("note: location=1 internal, state=1 enabled / 0 disabled-pending-user-approval")
