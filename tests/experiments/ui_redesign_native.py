"""Validate the redesigned UI inside an ALREADY RUNNING isolated Tauri WKWebView.

Usage:
  python3 tests/experiments/ui_redesign_native.py --driver /private/tmp/synchrome-ui-native/driver

The app must be a debug preview, launched with an isolated SYNCHROME_DATA_DIR,
SYNCHROME_DEV_EXTRA_DIRS_ONLY=1, SYNCHROME_DEV_REALTIME=off and empty SUPABASE_URL /
SUPABASE_ANON_KEY. The preview URL must include ?preview=workspace. The expected
data directory defaults to the driver's sibling ``data`` directory.

This script never launches/replaces an app, reads Keychain, logs in to a real account,
modifies real browser data, or calls native mutation commands. Its only Rust calls are
read-only metadata probes. Product interactions use the explicit in-memory DEV fixture
inside the actual Tauri window. Manual native pointer/keyboard and screenshot validation
complements this deterministic harness.
"""

import argparse
import json
import time
import uuid
from datetime import datetime, timezone
from pathlib import Path


JS = r"""
const preview = window.__SYNCHROME_PREVIEW__;
if (!preview || !window.__TAURI_INTERNALS__ || !window.__TAURI_INTERNALS__.metadata) {
  throw new Error('Refusing to run outside an explicit native development preview');
}
const snap = () => preview.snapshot();
const shown = el => !!el && el.getClientRects().length > 0 && getComputedStyle(el).visibility !== 'hidden';
const dialog = () => document.querySelector('dialog[open]');
const all = (selector, root = document) => [...root.querySelectorAll(selector)];
const text = el => el?.textContent.replace(/\s+/g, ' ').trim() || '';
const btn = (name, root = dialog() || document) => all('button', root).find(el => shown(el) &&
  (el.getAttribute('aria-label') === name || text(el) === name || text(el).startsWith(name)));
"""


class Driver:
    def __init__(self, directory):
        self.directory = directory.resolve()
        if not (self.directory / "in").is_dir():
            raise RuntimeError("An existing debug UI driver's input directory is required")
        if not (self.directory / "out").is_dir():
            raise RuntimeError("An existing debug UI driver's output directory is required")
        self.sequence = 0

    def evaluate(self, body, timeout=15, guarded=True):
        self.sequence += 1
        request_id = uuid.uuid4().hex
        wrapper = (
            "(async () => { let result; try { result = {ok:true, value:await (async () => {"
            + (JS if guarded else "")
            + body
            + "})()}; } catch(error) { result = {ok:false,error:String(error)}; }"
            + "await window.__TAURI_INTERNALS__.invoke('dev_ui_report', {id:"
            + json.dumps(request_id)
            + ",value:result}); })();"
        )
        target = self.directory / "in" / f"native-{self.sequence:06d}-{request_id}.js"
        temporary = target.with_suffix(".tmp")
        temporary.write_text(wrapper, encoding="utf-8")
        temporary.rename(target)
        result_path = self.directory / "out" / f"{request_id}.json"
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if result_path.exists():
                result = json.loads(result_path.read_text(encoding="utf-8"))
                result_path.unlink()
                if not result.get("ok"):
                    raise RuntimeError(result.get("error", "Native driver evaluation failed"))
                return result.get("value")
            time.sleep(0.1)
        raise TimeoutError("The existing native preview did not answer its debug UI driver")

    def wait(self, expression, timeout=20):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                if self.evaluate("return !!(" + expression + ");") is True:
                    return
            except RuntimeError as error:
                # The development preview object is recreated during a native page reload.
                # Other evaluation failures must still fail the test rather than look ready.
                if "Refusing to run outside an explicit native development preview" not in str(error):
                    raise
            time.sleep(0.15)
        raise AssertionError("Timed out waiting for: " + expression)

    def click(self, name, root="dialog() || document"):
        self.wait(f"btn({json.dumps(name)}, {root}) && !btn({json.dumps(name)}, {root}).disabled")
        self.evaluate(f"const el=btn({json.dumps(name)}, {root}); el.focus(); el.click(); return true;")

    def fill(self, selector, value):
        self.wait(f"shown(document.querySelector({json.dumps(selector)}))")
        self.evaluate("""
          const el=document.querySelector(%s); el.focus(); el.value=%s;
          el.dispatchEvent(new Event('input', {bubbles:true}));
          el.dispatchEvent(new Event('change', {bubbles:true})); return true;
        """ % (json.dumps(selector), json.dumps(value)))

    def choose(self, prefix):
        self.wait("shown(document.querySelector('#local-profile'))")
        self.evaluate("""
          const select=document.querySelector('#local-profile');
          const option=all('option',select).find(o=>!o.disabled && text(o).startsWith(%s));
          if(!option) throw new Error('Expected fixture browser option unavailable');
          select.value=option.value; select.dispatchEvent(new Event('change',{bubbles:true})); return true;
        """ % json.dumps(prefix))

    def navigate(self, route):
        self.wait(f"shown(document.querySelector('[data-testid=nav-{route}]'))")
        self.evaluate(f"document.querySelector('[data-testid=nav-{route}]').click(); return true;")
        self.wait(f"document.querySelector('[data-testid=nav-{route}]')?.getAttribute('aria-current')==='page'")

    def profile(self, name):
        self.navigate("profiles")
        self.click("Open profile " + name, "document")
        self.wait(f"text(document.querySelector('.profile-detail h2'))==={json.dumps(name)}")

    def scenario(self, name):
        if name not in {"workspace", "auth", "onboarding", "offline", "empty"}:
            raise ValueError("Unknown fixture scenario")
        self.evaluate("""
          const url=new URL(location.href); url.searchParams.set('preview',%s);
          setTimeout(()=>location.assign(url.href),100); return true;
        """ % json.dumps(name))
        time.sleep(0.4)
        self.wait(f"snap().scenario==={json.dumps(name)} && !document.querySelector('.boot')")


class Suite:
    def __init__(self, driver):
        self.driver = driver
        self.checks = []

    def check(self, name, expression):
        passed = self.driver.evaluate("return !!(" + expression + ");") is True
        self.checks.append({"name": name, "passed": passed})
        print(("PASS " if passed else "FAIL ") + name, flush=True)
        if not passed:
            raise AssertionError(name)

    def case(self, name, run):
        try:
            run()
        except Exception as error:
            self.checks.append({"name": name + " completed", "passed": False, "detail": str(error)})
            print("FAIL " + name + ": " + str(error), flush=True)

    def workspace(self):
        d = self.driver
        d.scenario("workspace")
        self.check("Native overview shows three central profiles", "all('.profile-overview-row').length===3")
        self.check("Navigation exposes the five product areas", "all('nav button').map(b=>b.getAttribute('aria-label')).join(',')==='Overview,Profiles,Devices,Activity,Settings'")
        self.check("Mixed attention names incomplete connections without claiming all are syncing", "text(document.querySelector('.attention-strip')).includes('Other connections also need setup or recovery')")
        d.click("Review conflicts", "document.querySelector('.attention-strip')")
        self.check("The attention action opens the conflict screen it describes", "document.querySelector('[data-testid=nav-activity]').getAttribute('aria-current')==='page'")
        d.profile("Work")
        self.check("Profile relationships group browsers under two Macs", "all('.device-group').length===2 && all('.connection').length===2")
        self.check("Remote connections do not claim a live synced state", "text(document.querySelector('.remote-note')).includes('does not show its live status')")
        self.check("A second local source cannot be connected silently", "btn('Connect browser',document).disabled")
        self.check("Primary profile view hides internal identifiers", "!document.querySelector('.profile-detail').innerText.match(/[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}/)")
        d.click("New profile", "document")
        self.check("Native modal has a labelled title and contains focus", "dialog().getAttribute('aria-labelledby') && document.getElementById(dialog().getAttribute('aria-labelledby')) && dialog().contains(document.activeElement)")
        d.evaluate("dialog().dispatchEvent(new Event('cancel',{cancelable:true})); return true;")
        d.wait("!dialog()")
        self.check("Dialog cancellation restores the invoking control", "text(document.activeElement).includes('New profile')")

    def create_connect(self):
        d = self.driver
        d.scenario("workspace")
        d.click("New profile", "document")
        d.click("Start fresh")
        d.click("Create profile")
        d.wait("text(document.querySelector('#profile-error')).includes('name')")
        self.check("Blank profile names have an inline validation error", "document.querySelector('#profile-name').getAttribute('aria-invalid')==='true'")
        d.fill("#profile-name", " personal ")
        d.click("Create profile")
        d.wait("text(document.querySelector('#profile-error')).includes('already')")
        self.check("Duplicate names are rejected before creating a profile", "snap().profiles.length===3")
        d.fill("#profile-name", "Native review")
        d.click("Create profile")
        d.wait("!dialog() && snap().profiles.some(p=>p.name==='Native review')")
        self.check("Starting fresh does not connect or change a browser", "snap().mappings.length===5 && all('.profile-detail .connection').length===0")
        d.click("Connect browser", "document")
        d.choose("Helium — Reading")
        d.click("Connect browser")
        d.wait("dialog() && text(dialog().querySelector('h2'))==='Finish connecting your browser'")
        self.check("Mapping stays pending until the companion confirms it", "snap().mappings.find(m=>m.profile_name==='Native review').display_status==='token_written' && !snap().mappings.find(m=>m.profile_name==='Native review').bookmarks")
        d.evaluate("preview.confirmMapping(snap().mappings.find(m=>m.profile_name==='Native review').id); return true;")
        d.click("Check connection")
        d.wait("text(dialog()).includes('Waiting for first sync')")
        self.check("Companion confirmation alone does not announce first sync", "!text(dialog().querySelector('h2')).includes('complete')")
        d.click("Sync now")
        d.wait("text(dialog()?.querySelector('h2'))==='Your first sync is complete.'")
        self.check("First sync completion follows actual observed fixture sync", "snap().mappings.find(m=>m.profile_name==='Native review').bookmarks.last_upload_at>0")
        d.click("View profile")
        d.click("Rename Native review", "document")
        d.fill("#rename-profile", "Reading room")
        d.click("Save name")
        d.wait("!dialog() && text(document.querySelector('.profile-detail h2'))==='Reading room'")
        self.check("Renaming preserves the existing browser relationship", "snap().mappings.some(m=>m.profile_name==='Reading room')")
        d.click("Disconnect", "document.querySelector('.profile-detail')")
        self.check("Disconnect explains that local browser data is kept", "text(dialog()).includes('local profile, bookmarks and files are kept')")
        d.click("Disconnect browser")
        d.wait("!dialog() && !snap().mappings.some(m=>m.profile_name==='Reading room')")
        self.check("Disconnect retains the central profile", "snap().profiles.some(p=>p.name==='Reading room')")

    def import_onboarding(self):
        d = self.driver
        d.scenario("onboarding")
        self.check("First entry explains the browser to central-profile relationship", "text(dialog()).includes('Your browser') && text(dialog()).includes('Synchrome profile') && text(dialog()).includes('Other Macs')")
        d.click("Choose a browser")
        self.check("Permission guidance is specific and offers Helium without access", "text(dialog()).includes('Full Disk Access') && text(dialog()).includes('Helium can be used without')")
        d.choose("Helium — Personal")
        d.fill("#profile-name", "Imported browsing")
        d.click("Import profile")
        d.wait("snap().profiles.length===1 && snap().mappings.length===1 && text(dialog()?.querySelector('h2'))==='Finish connecting your browser'")
        self.check("Import creates a core profile and maps its chosen local origin", "snap().mappings[0].profile_id===snap().profiles[0].id && snap().mappings[0].browser_kind==='helium' && snap().mappings[0].browser_profile_dir==='Default'")
        self.check("Import does not invent a completed sync", "snap().mappings[0].bookmarks===null && text(dialog()).includes('Confirm your first sync')")
        d.click("Install connector")
        d.wait("text(dialog()).includes('Connector installed')")
        self.check("Companion instructions show a concrete setup path", "text(dialog()).includes('chrome://extensions') && text(dialog()).includes('Load unpacked') && text(dialog()).includes('Show companion folder')")
        d.click("Continue later")
        d.navigate("profiles")
        self.check("Incomplete setup remains discoverable on its profile", "text(document.querySelector('.profile-detail')).includes('Finish setup')")

    def browser_running(self):
        d = self.driver
        d.scenario("workspace")
        d.click("New profile", "document")
        d.click("Use a browser profile")
        d.choose("Google Chrome — Client studio")
        d.fill("#profile-name", "Open-browser import")
        self.check("An open browser produces guidance before connecting", "text(dialog()).includes('Close Google Chrome before connecting')")
        d.click("Import profile")
        d.wait("snap().mappings.some(m=>m.profile_name==='Open-browser import') && text(dialog()).includes('Retry link')")
        self.check("The pending browser link does not claim sync is active", "snap().mappings.find(m=>m.profile_name==='Open-browser import').display_status==='error'")
        d.evaluate("preview.setBrowserAccess('chrome','accessible'); return true;")
        d.click("Retry link")
        d.wait("snap().mappings.find(m=>m.profile_name==='Open-browser import').display_status==='token_written'")
        d.wait("text(dialog()).includes('Load the Synchrome companion')")
        self.check("Retry advances to companion setup after the browser closes", "text(dialog()).includes('Load the Synchrome companion')")

    def existing_profiles_onboarding(self):
        d = self.driver
        d.scenario("workspace")
        d.evaluate("""
          for (const mapping of snap().mappings.filter(m=>m.is_this_device)) {
            await preview.invoke('unmap_local_profile',{mappingId:mapping.id});
          }
          return true;
        """)
        d.navigate("settings")
        d.click("Sign out", "document")
        d.click("Sign out")
        d.wait("shown(document.querySelector('form.login'))")
        d.fill("form.login input[type=email]", "review@example.test")
        d.fill("form.login input[type=password]", "fixture-password")
        d.click("Sign in")
        d.wait("dialog() && text(dialog()).includes('Bring your profiles to this Mac')")
        self.check("A new Mac offers existing profiles instead of requiring an import", "snap().profiles.length===3 && btn('Connect a profile') && !btn('Choose a browser')")
        d.click("Connect a profile")
        self.check("Existing-profile onboarding opens the shared profile selector", "shown(document.querySelector('#target-profile')) && !document.querySelector('#profile-name')")
        personal_id = d.evaluate("return snap().profiles.find(p=>p.name==='Personal').id;")
        d.fill("#target-profile", personal_id)
        d.choose("Helium — Personal")
        d.click("Connect browser")
        d.wait("text(dialog()?.querySelector('h2'))==='Finish connecting your browser'")
        self.check("Connecting another Mac reuses the profile without duplicating it", "snap().profiles.length===3 && snap().mappings.some(m=>m.profile_name==='Personal'&&m.is_this_device)")
        d.click("Continue later")

    def deletion(self):
        d = self.driver
        d.scenario("workspace")
        before = d.evaluate("return JSON.stringify(snap().browserData);")
        d.profile("Personal")
        d.click("Delete Personal", "document")
        self.check("Delete explicitly distinguishes cloud and local data", "text(dialog()).includes('Removed from Synchrome') && text(dialog()).includes('Your browser data stays')")
        self.check("Delete requires the exact profile name", "btn('Delete profile').disabled")
        d.fill("#delete-name", "Wrong name")
        self.check("A mismatched confirmation cannot delete", "btn('Delete profile').disabled")
        d.fill("#delete-name", "Personal")
        d.evaluate("preview.failNext('unmap_local_profile','network connection failed'); return true;")
        d.click("Delete profile")
        d.wait("document.querySelector('dialog[open] .form-error') && btn('Delete profile') && !btn('Delete profile').disabled")
        self.check("A failed disconnection keeps the cloud profile for retry", "snap().profiles.some(p=>p.name==='Personal') && snap().mappings.filter(m=>m.profile_name==='Personal').length===2")
        d.click("Delete profile")
        d.wait("!dialog() && !snap().profiles.some(p=>p.name==='Personal')")
        self.check("Delete removes all mappings and profile cloud files", "!snap().mappings.some(m=>m.profile_name==='Personal') && Object.keys(snap().cloudObjects).length===2")
        self.check("Deleting a cloud profile preserves every browser fixture", "JSON.stringify(snap().browserData)===" + json.dumps(before))

        d.scenario("workspace")
        d.profile("Personal")
        d.click("Delete Personal", "document")
        d.fill("#delete-name", "Personal")
        d.evaluate("""
          const original=preview.invoke.bind(preview);
          preview.invoke=async(command,args)=>{
            if(command==='delete_profile'){
              preview.invoke=original; await original(command,args);
              throw new Error('The Synchrome profile was deleted, but cleanup is incomplete. Cloud file cleanup could not be verified.');
            }
            return original(command,args);
          }; return true;
        """)
        d.click("Delete profile")
        d.wait("!dialog() && document.querySelector('main [role=alert]') && !snap().profiles.some(p=>p.name==='Personal')")
        self.check("Incomplete cloud cleanup does not appear as full deletion success", "text(document.querySelector('main [role=alert]')).includes('profile was deleted') && text(document.querySelector('main [role=alert]')).includes('local browser')")

    def conflicts(self):
        d = self.driver
        d.scenario("workspace")
        d.navigate("activity")
        d.wait("all('.conflict').length===1")
        self.check("Conflict comparison presents local and remote versions", "text(document.querySelector('.conflict')).includes('Project handbook') && text(document.querySelector('.conflict')).includes('Team handbook')")
        d.click("Keep this device's version", "document.querySelector('.conflict')")
        d.wait("snap().mappings.find(m=>m.profile_name==='Work'&&m.is_this_device).bookmarks.download.conflicts===0")
        self.check("Conflict decisions update the actual fixture state", "Object.values(snap().conflicts).flat().some(c=>c.status==='resolved'&&c.resolution==='keep_local')")

    def auth(self):
        d = self.driver
        d.scenario("auth")
        self.check("Sign in is a labelled native desktop form", "shown(document.querySelector('form.login')) && all('form.login label').length===2 && btn('Sign in').disabled")
        d.fill("form.login input[type=email]", "invalid-email")
        d.fill("form.login input[type=password]", "fixture-password")
        d.click("Sign in")
        d.wait("text(document.querySelector('form.login')).includes('Enter a valid email')")
        self.check("Validation focuses the field needing correction", "document.activeElement.type==='email' && !snap().account.signed_in")
        d.fill("form.login input[type=email]", "review@example.test")
        d.evaluate("preview.failNext('sign_in','invalid email or password'); return true;")
        d.click("Sign in")
        d.wait("document.querySelector('.auth-error')")
        self.check("Native auth errors get specific plain-language feedback", "text(document.querySelector('.auth-error')).includes('password') && !text(document.querySelector('.auth-error')).startsWith('Something went wrong')")
        d.click("Create an account")
        d.fill("form.login input[type=password]", "short")
        d.click("Create account")
        d.wait("text(document.querySelector('form.login')).includes('at least 8')")
        self.check("Signup has immediate password validation", "!snap().account.signed_in")
        d.fill("form.login input[type=password]", "fixture-password")
        d.evaluate("""
          const original=preview.invoke.bind(preview);
          preview.invoke=async(command,args)=>{
            if(command==='sign_up'){
              preview.invoke=original;
              return {...snap().account,error:'Account created. Confirm the e-mail we sent you, then sign in.'};
            }
            return original(command,args);
          }; return true;
        """)
        d.click("Create account")
        d.wait("text(document.querySelector('.confirmation')).includes('Check your inbox')")
        self.check("Email confirmation is shown as pending success", "!document.querySelector('form.login') && btn('Back to sign in')")
        d.click("Back to sign in")
        d.fill("form.login input[type=password]", "fixture-password")
        d.click("Sign in")
        d.wait("document.querySelector('nav') && dialog()")
        self.check("Successful sign in enters progressive onboarding", "snap().account.signed_in && text(dialog()).includes('Bring your browsing together')")
        d.click("Explore workspace")
        d.navigate("settings")
        d.click("Sign out", "document")
        self.check("Sign out explains pause and preserved data", "text(dialog()).includes('Sync pauses') && text(dialog()).includes('browser data and shared profiles are kept')")
        d.click("Sign out")
        d.wait("shown(document.querySelector('form.login'))")
        self.check("Signing out returns to the auth form", "!snap().account.signed_in && !document.querySelector('nav')")

    def states_appearance(self):
        d = self.driver
        d.scenario("offline")
        self.check("Offline has a clear data-preservation notice", "document.querySelector('main').innerText.includes('Your local changes are kept')")
        d.profile("Personal")
        self.check("Offline connection state does not claim synced", "text(document.querySelector('.profile-detail')).includes('Offline')")
        d.scenario("empty")
        d.click("Explore workspace")
        self.check("Empty state offers import and starting fresh", "text(document.querySelector('.empty-state')).includes('Use a browser profile') && text(document.querySelector('.empty-state')).includes('Start fresh')")
        d.scenario("workspace")
        d.navigate("settings")
        d.click("Dark", "document.querySelector('.appearance-options')")
        d.wait("document.documentElement.dataset.theme==='dark'")
        self.check("Dark appearance is applied and its selection announced", "btn('Dark',document.querySelector('.appearance-options')).getAttribute('aria-pressed')==='true' && document.documentElement.style.colorScheme==='dark'")
        d.click("Light", "document.querySelector('.appearance-options')")
        d.wait("document.documentElement.dataset.theme==='light'")
        self.check("Light appearance is applied and persisted", "localStorage.getItem('synchrome.appearance')==='light'")
        d.navigate("overview")
        self.check("Current native window has no horizontal page overflow", "document.documentElement.scrollWidth<=document.documentElement.clientWidth+1")
        self.check("Visible controls have accessible names", "all('button').filter(shown).every(b=>b.getAttribute('aria-label')||text(b))")
        self.check("Preview banner leaves the account control inside the native viewport", "document.querySelector('.account-button').getBoundingClientRect().bottom<=innerHeight+1")
        for route in ["profiles", "devices", "activity", "settings"]:
            d.navigate(route)
            self.check("Native " + route + " fits the supported window width", "document.documentElement.scrollWidth<=document.documentElement.clientWidth+1")

    def cloud_truth(self):
        d = self.driver
        d.scenario("workspace")
        d.evaluate("""
          const original=preview.invoke.bind(preview);
          preview.invoke=async(command,args)=>command==='cloud_status'?null:original(command,args);
          return true;
        """)
        d.click("Refresh workspace", "document")
        d.wait("text(document.querySelector('.service-status')).includes('Checking cloud')")
        self.check("A pending cloud probe does not appear connected", "document.querySelector('.service-status .status.info') && !document.querySelector('.service-status .status.positive')")
        d.evaluate("""
          const original=preview.invoke.bind(preview);
          preview.invoke=async(command,args)=>command==='cloud_status'
            ?{configured:true,reachable:true,auth_ok:true,rest_ok:false,storage_ok:true}
            :original(command,args);
          return true;
        """)
        d.click("Refresh workspace", "document")
        d.wait("text(document.querySelector('.service-status')).includes('Cloud partially available')")
        self.check("Partial cloud availability uses an attention state", "!!document.querySelector('.service-status .status.warning')")
        d.navigate("settings")
        d.evaluate("document.querySelector('details.diagnostics').open=true; return true;")
        self.check("Settings and shell agree on partial cloud availability", "text(document.querySelector('.diagnostics .status.warning')).includes('Cloud partially available')")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--driver", type=Path, required=True)
    parser.add_argument("--expected-data-dir", type=Path)
    parser.add_argument("--report", type=Path, default=Path("docs/ui/native-validation.json"))
    args = parser.parse_args()
    driver = Driver(args.driver)
    expected_dir = (args.expected_data_dir or driver.directory.parent / "data").resolve()
    if not expected_dir.is_relative_to(driver.directory.parent):
        raise RuntimeError("Expected native data must be isolated alongside this debug driver")
    device_file = expected_dir / "device.json"
    expected_device = json.loads(device_file.read_text(encoding="utf-8"))
    driver.wait("!!preview && !document.querySelector('.boot')")
    initial = driver.evaluate("""
      return {scenario:snap().scenario,native:!!window.__TAURI_INTERNALS__.metadata,
        preview:new URLSearchParams(location.search).get('preview'),
        fixtureOnly:snap().browsers.every(b=>b.user_data_dir.startsWith('/Preview/')),
        tokenFree:!JSON.stringify(snap()).match(/\"(?:access_token|refresh_token|mapping_token)\"/)};
    """)
    if not initial["native"] or not initial["fixtureOnly"] or not initial["tokenFree"] or initial["preview"] not in {"workspace", "auth", "onboarding", "offline", "empty"}:
        raise RuntimeError("Native fixture safety gate failed; no interactions were performed")
    native = driver.evaluate("""
      const invoke=window.__TAURI_INTERNALS__.invoke;
      const account=await invoke('account_status'),cloud=await invoke('cloud_status');
      if(account.signed_in || cloud.configured) throw new Error('Native backend must be unconfigured and signed out');
      const device=await invoke('device_identity');
      if(device.device_id!==%s) throw new Error('Native device does not match the isolated data directory');
      const browsers=await invoke('list_browsers');
      if(browsers.length) throw new Error('Isolated native discovery must return no browser profiles');
      return {version:await invoke('app_version'),nativeSignedOut:!account.signed_in,
        nativeCloudUnconfigured:!cloud.configured,isolatedDevice:true,nativeBrowserCount:browsers.length};
    """ % json.dumps(expected_device["device_id"]))
    suite = Suite(driver)
    for label, case in [
        ("Workspace and modal semantics", suite.workspace),
        ("Create, connect, first sync, rename, disconnect", suite.create_connect),
        ("Progressive onboarding and browser import", suite.import_onboarding),
        ("Open browser and retry recovery", suite.browser_running),
        ("Existing profiles on a new Mac", suite.existing_profiles_onboarding),
        ("Safe profile deletion and partial failures", suite.deletion),
        ("Conflict decisions", suite.conflicts),
        ("Auth, signup, confirmation and logout", suite.auth),
        ("Offline, empty states and appearance", suite.states_appearance),
        ("Truthful cloud health", suite.cloud_truth),
    ]:
        suite.case(label, case)
    # Restore a useful, predictable native preview for manual review after the run.
    driver.scenario("workspace")
    driver.navigate("settings")
    driver.click("Light", "document.querySelector('.appearance-options')")
    driver.navigate("overview")
    geometry = driver.evaluate("return {width:innerWidth,height:innerHeight,pageWidth:document.documentElement.scrollWidth,narrowWindow:innerWidth<=740};")
    report = {
        "generatedAt": datetime.now(timezone.utc).isoformat(),
        "runtime": "Tauri 2 / native macOS WKWebView",
        "driver": str(driver.directory),
        "scope": "Real desktop frontend and dialogs; auth/cloud/companion operations use token-free DEV fixtures",
        "nativeMetadata": native,
        "geometry": geometry,
        "checks": suite.checks,
        "passed": sum(c["passed"] for c in suite.checks),
        "failed": sum(not c["passed"] for c in suite.checks),
        "limitations": [
            "No live cloud auth, real browser data, OS permission grants or companion installation were exercised.",
            "Native pointer/keyboard interaction and screenshots are validated separately through Computer Use.",
            "Window overflow is measured at the actual native size recorded in geometry.",
        ],
    }
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(f"Native validation: {report['passed']} passed, {report['failed']} failed. Report: {args.report}", flush=True)
    return 1 if report["failed"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
