"""E13b — follow-ups of E13 (DISPOSABLE profiles only).

  a) bookmark_bar.show_on_all_tabs: where else is it stored, and does an external edit to
     true / to false stick (from default, and after an API set)?
  b) languages: does an external edit of intl.selected_languages stick and drive
     intl.accept_languages? does accept_languages alone get recomputed?
  c) spellcheck.dictionaries: kept when its languages are among the selected languages?

  python3 e13b_prefs_detail.py helium|chrome
"""
import json, os, shutil, subprocess, sys, tempfile, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import e13_preferences as e  # noqa: E402
from cdp import CDP  # noqa: E402

KEYS = ["bookmark_bar.show_on_all_tabs", "intl.selected_languages", "intl.accept_languages", "spellcheck.dictionaries", "download.prompt_for_download"]


def effective(c, s):
    return e.ev(c, s, "Promise.all(%s.map(k => new Promise(r => chrome.settingsPrivate.getPref(k, p => r(p ? p.value : null)))))" % json.dumps(KEYS))


def occurrences(udd):
    hits = {}
    for root, _, files in os.walk(udd):
        for f in files:
            p = os.path.join(root, f)
            try:
                b = open(p, "rb").read()
            except OSError:
                continue
            if b"show_on_all_tabs" in b:
                hits[os.path.relpath(p, udd)] = b.count(b"show_on_all_tabs")
    return hits


def run(bin_, udd, edit=None, api=None):
    """Optionally edit Preferences (closed) first, launch, optionally set via API, read, quit."""
    prof = os.path.join(udd, "Default")
    if edit:
        pr = json.load(open(os.path.join(prof, "Preferences")))
        for k, v in edit.items():
            e.put(pr, k, v)
        json.dump(pr, open(os.path.join(prof, "Preferences"), "w"), separators=(",", ":"))
    p = e.launch(bin_, udd)
    c = CDP(udd)
    s = e.settings_session(c)
    time.sleep(1)
    eff = dict(zip(KEYS, effective(c, s) or []))
    if api:
        for k, v in api.items():
            e.ev(c, s, "new Promise(r => chrome.settingsPrivate.setPref(%s, %s, '', ok => r(ok)))" % (json.dumps(k), json.dumps(v)))
        time.sleep(1)
    e.quit_(p, c)
    pr = json.load(open(os.path.join(prof, "Preferences")))
    return eff, {k: e.get(pr, k) for k in KEYS}, pr.get("bookmark_bar")


def main(kind):
    bin_ = e.BINS[kind]
    udd = tempfile.mkdtemp(prefix=f"synchrome-e13-{kind}-")
    try:
        steps = [
            ("fresh", None, None),
            ("edit show_on_all_tabs=true from default", {"bookmark_bar.show_on_all_tabs": True}, None),
            ("edit show_on_all_tabs=false", {"bookmark_bar.show_on_all_tabs": False}, None),
            ("api set show_on_all_tabs=true", None, {"bookmark_bar.show_on_all_tabs": True}),
            ("edit show_on_all_tabs=false after api", {"bookmark_bar.show_on_all_tabs": False}, None),
            ("edit selected_languages=es,en", {"intl.selected_languages": "es,en"}, None),
            ("edit accept_languages=fr only", {"intl.accept_languages": "fr"}, None),
            ("edit dictionaries=[es] (es selected)", {"spellcheck.dictionaries": ["es"]}, None),
            ("edit dictionaries=[de] (de not selected)", {"spellcheck.dictionaries": ["de"]}, None),
            ("edit prompt_for_download=true", {"download.prompt_for_download": True}, None),
        ]
        for name, edit, api in steps:
            eff, file_, bb = run(bin_, udd, edit, api)
            print(f"--- {name}\n  effective at launch: {json.dumps(eff)}\n  file after quit:     {json.dumps(file_)}\n  bookmark_bar block:  {json.dumps(bb)}")
        print("files mentioning show_on_all_tabs:", occurrences(udd))
        print("Last Version:", open(os.path.join(udd, "Last Version")).read().strip())
    finally:
        for p_ in e.PROCS:
            if p_ and p_.poll() is None:
                p_.kill()
                p_.wait(10)
        shutil.rmtree(udd, ignore_errors=True)


if __name__ == "__main__":
    main(sys.argv[1])
