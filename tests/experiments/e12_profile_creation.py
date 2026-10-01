"""E12 — Can we create a Chromium profile from outside and name it?

Steps: fresh user-data-dir → launch with --profile-directory="Profile 7" headless → quit →
check Local State info_cache + dir → rename via Local State while closed → relaunch → verify.
Usage: python3 e12_profile_creation.py "<browser binary>" <label>
"""
import json, os, shutil, signal, subprocess, sys, time

bin_, name = sys.argv[1], sys.argv[2]
base = os.path.dirname(os.path.abspath(__file__))
udd = os.path.join(base, f"udd-e12-{name}")
shutil.rmtree(udd, ignore_errors=True)
ls_path = os.path.join(udd, "Local State")


def run(secs, *extra):
    p = subprocess.Popen([bin_, f"--user-data-dir={udd}", "--headless=new", "--no-first-run",
                          "--no-default-browser-check", "--disable-gpu", *extra, "about:blank"],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(secs)
    p.send_signal(signal.SIGTERM)
    p.wait(20)


def local_state():
    return json.load(open(ls_path))


print(f"=== E12 {name} ===")
run(10, '--profile-directory=Profile 7')
ls = local_state()
cache = ls.get("profile", {}).get("info_cache", {})
print("profile dirs created:", sorted(d for d in os.listdir(udd) if d.startswith("Profile") or d == "Default"))
print("info_cache keys:", sorted(cache.keys()))
entry = cache.get("Profile 7")
print("Profile 7 entry present:", entry is not None, "| name:", entry and entry.get("name"),
      "| is_using_default_name:", entry and entry.get("is_using_default_name"))

# Rename while closed
if entry is not None:
    entry["name"] = "Synchrome Test"
    entry["is_using_default_name"] = False
    ls["profile"]["last_used"] = "Profile 7"
    json.dump(ls, open(ls_path, "w"))
    run(10, '--profile-directory=Profile 7')
    ls2 = local_state()
    e2 = ls2["profile"]["info_cache"].get("Profile 7", {})
    print("after relaunch: name =", e2.get("name"), "| is_using_default_name =", e2.get("is_using_default_name"),
          "| last_used =", ls2["profile"].get("last_used"))
    print("Preferences exists:", os.path.exists(os.path.join(udd, "Profile 7", "Preferences")))
    # Second profile in same run, to confirm sequential creation works too
    run(8, '--profile-directory=Profile 8')
    print("after Profile 8 launch, info_cache keys:", sorted(local_state()["profile"]["info_cache"].keys()))
