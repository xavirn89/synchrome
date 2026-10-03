"""Drive the RELEASE Synchrome app (no dev driver) through macOS accessibility — v1 closure.

WKWebView exposes the page to assistive clients once `AXEnhancedUserInterface` is set on the
process. Values are set through AX (`set value`), never typed or put on the clipboard; scripts
go to osascript through stdin, so no secret appears in a process list. Read-only helpers return
the visible texts (counts/labels), never anything the UI does not show.
"""
import json, os, subprocess, time


def q(x):
    """An AppleScript string literal (UTF-8 kept: AppleScript has no \\u escapes)."""
    return json.dumps(x, ensure_ascii=False)

APP = "/Applications/Synchrome.app"
APP_DATA = os.path.expanduser("~/Library/Application Support/Synchrome")

PRELUDE = '''
tell application "Synchrome" to activate
delay 0.5
tell application "System Events"
  tell process "Synchrome"
    try
      set value of attribute "AXEnhancedUserInterface" to true
    end try
    delay 1
    set w to window 1
    set els to entire contents of w
'''
EPILOGUE = '''
  end tell
end tell
'''


def osa(body, timeout=60):
    # WebKit rebuilds its accessibility tree on re-render; the window is briefly not enumerable
    for attempt in range(4):
        p = subprocess.run(["osascript", "-"], input=(PRELUDE + body + EPILOGUE).encode("utf-8"), capture_output=True, timeout=timeout)
        if p.returncode == 0:
            return p.stdout.decode(errors="replace").rstrip("\n")
        err = p.stderr.decode(errors="replace").strip()[:300]
        if "-1719" not in err or attempt == 3:
            raise RuntimeError(err)
        time.sleep(1.5)


def texts():
    """Every visible text / button label of the window, in order."""
    out = osa('''
    set out to ""
    repeat with e in els
      try
        set r to role of e
        if r is in {"AXStaticText", "AXButton", "AXHeading", "AXPopUpButton"} then
          set n to ""
          try
            set n to name of e
          end try
          set out to out & r & "\\t" & n & linefeed
        end if
      end try
    end repeat
    return out
    ''')
    return [tuple(l.split("\t", 1)) for l in out.splitlines() if "\t" in l]


def text_blob():
    return "\n".join(n for _, n in texts())


def click(name, nth=1):
    """Click the nth button whose label starts with `name`."""
    return osa(f'''
    set k to 0
    repeat with e in els
      try
        if role of e is "AXButton" and (name of e) starts with {q(name)} then
          set k to k + 1
          if k is {nth} then
            click e
            return "clicked"
          end if
        end if
      end try
    end repeat
    return "not found"
    ''')


def click_after(anchor, name):
    """Click the first button labelled `name…` that comes AFTER the static text `anchor` (in
    document order) — e.g. a button inside one specific browser entry. Never anything before."""
    return osa(f'''
    set seen to false
    repeat with e in els
      try
        set r to role of e
        if (not seen) and r is "AXStaticText" and (name of e) is {q(anchor)} then set seen to true
        if seen and r is "AXButton" and (name of e) starts with {q(name)} then
          click e
          return "clicked"
        end if
      end try
    end repeat
    return "not found"
    ''')


def count_buttons(name):
    return int(osa(f'''
    set k to 0
    repeat with e in els
      try
        if role of e is "AXButton" and (name of e) starts with {q(name)} then set k to k + 1
      end try
    end repeat
    return k as text
    ''') or 0)


def set_field(label, value):
    """Set a text field found by its accessible name (the <label> text)."""
    return osa(f'''
    repeat with e in els
      try
        if role of e is in {{"AXTextField", "AXSecureTextField"}} and (name of e) is {q(label)} then
          set focused of e to true
          set value of e to {q(value)}
          return "set"
        end if
      end try
    end repeat
    return "not found"
    ''')


def type_field(label, value):
    """Focus a field by clicking it and type the value (fires the page's input events)."""
    return osa(f'''
    set frontmost to true
    delay 0.3
    repeat with e in els
      try
        if role of e is in {{"AXTextField", "AXSecureTextField"}} and (name of e) is {q(label)} then
          click e
          delay 0.3
          keystroke "a" using command down
          keystroke {q(value)}
          return "typed"
        end if
      end try
    end repeat
    return "not found"
    ''')


def wait_text(pred, timeout=60, step=1.0):
    end = time.time() + timeout
    blob = ""
    while time.time() < end:
        try:
            blob = text_blob()
            if pred(blob):
                return blob
        except RuntimeError:
            pass
        time.sleep(step)
    return None


def launch():
    subprocess.run(["open", "-a", APP], check=True)
    for _ in range(60):
        if running():
            break
        time.sleep(0.5)
    time.sleep(3)


def running():
    r = subprocess.run(["pgrep", "-f", APP + "/Contents/MacOS/synchrome-desktop"], capture_output=True, text=True)
    return [int(x) for x in r.stdout.split()]


def quit_app():
    subprocess.run(["osascript", "-e", 'tell application "Synchrome" to quit'], capture_output=True, timeout=30)
    for _ in range(40):
        if not running():
            return True
        time.sleep(0.5)
    for pid in running():
        os.kill(pid, 15)
    time.sleep(2)
    return not running()


def rt():
    try:
        return json.load(open(os.path.join(APP_DATA, "realtime-status.json")))
    except (OSError, ValueError):
        return {}


def keychain_has_session():
    return subprocess.run(["security", "find-generic-password", "-s", "com.synchrome.app", "-a", "session"], capture_output=True).returncode == 0


def creds(path, who):
    v = {}
    for line in open(path, encoding="utf-8"):
        k, _, x = line.strip().removeprefix("export ").partition("=")
        v[k.strip()] = x.strip().strip('"').strip("'")
    return v.get(f"USER_{who}_EMAIL", ""), v.get(f"USER_{who}_PASSWORD", "")
