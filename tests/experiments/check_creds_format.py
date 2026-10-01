"""Pre-check a USER_A/USER_B credentials env file WITHOUT printing any value.
Usage: python3 check_creds_format.py /tmp/synchrome-slice5-users.env"""
import re, sys
text = open(sys.argv[1], encoding="utf-8").read()
for k in ["USER_A_EMAIL", "USER_A_PASSWORD", "USER_B_EMAIL", "USER_B_PASSWORD"]:
    line = next((l for l in text.splitlines() if re.match(rf"\s*(export\s+)?{k}=", l)), None)
    if line is None:
        print(f"{k}: MISSING"); continue
    raw = line.split("=", 1)[1]
    v = raw.strip()
    quoted = len(v) >= 2 and v[0] == v[-1] and v[0] in "\"'"
    inner = v[1:-1] if quoted else v
    info = dict(present=True, empty=(inner == ""), quoted=quoted,
                export_prefix=line.lstrip().startswith("export "), crlf=raw.endswith("\r"),
                inner_whitespace=(inner != inner.strip()), length=len(inner))
    if k.endswith("EMAIL"):
        info["looks_like_email"] = bool(re.fullmatch(r"[^@\s]+@[^@\s]+\.[^@\s]+", inner))
    print(f"{k}: " + ", ".join(f"{a}={b}" for a, b in info.items()))
