"""Probe Lucide/Morphicons in an already running, isolated native DEV fixture."""
import argparse
import json
from pathlib import Path
from ui_redesign_native import Driver

parser = argparse.ArgumentParser()
parser.add_argument('--driver', type=Path, required=True)
d = Driver(parser.parse_args().driver)
d.scenario('auth')
probe = r'''
const button = btn('Show password') || btn('Hide password');
const svg = button.querySelector('svg');
let mutations = 0;
const observer = new MutationObserver(records => mutations += records.filter(r => r.attributeName === 'd').length);
observer.observe(svg, {subtree:true, attributes:true, attributeFilter:['d']});
button.dispatchEvent(new MouseEvent('click', {bubbles:true, detail:DETAIL}));
await new Promise(resolve => setTimeout(resolve, 650));
observer.disconnect();
return {mutations, inputType:document.querySelector('input[name=password]').type, hidden:svg.getAttribute('aria-hidden')};
'''
keyboard = d.evaluate(probe.replace('DETAIL', '0'))
pointer = d.evaluate(probe.replace('DETAIL', '1'))
assert keyboard['mutations'] <= 2 and keyboard['inputType'] == 'text', keyboard
assert pointer['mutations'] > 2 and pointer['inputType'] == 'password', pointer
assert keyboard['hidden'] == 'true' and pointer['hidden'] == 'true'
report = {'runtime':'Tauri WKWebView', 'scope':'Isolated in-memory auth fixture, DOM mouse events; no real account or browser writes', 'keyboard':keyboard, 'pointer':pointer, 'passed':True}
Path('docs/ui/icons-motion-native-validation.json').write_text(json.dumps(report, indent=2)+'\n')
print(json.dumps(report))
