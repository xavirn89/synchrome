# Experiments

Research scripts from the planning phase (see `docs/01-chromium-internals.md` §9). They launch a browser
headless with a throwaway `--user-data-dir` next to the script, so no real profile is touched.

```bash
python3 tracked_prefs.py "/Applications/Helium.app/Contents/MacOS/Helium" helium
python3 history_bookmarks.py "/Applications/Helium.app/Contents/MacOS/Helium" helium
```

Results on 2026-10-01 (macOS 27.0.1): tracked prefs reset in Helium 0.18.2.1 / Chrome / Brave;
untracked prefs kept; externally inserted History rows and a Bookmarks node survived relaunch.
Milestone 0 adds E1–E11 from `docs/06-v1-plan.md` here.
