"""Publish local HEAD to GitHub through the REST API (git push needs github.com)."""
import base64
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

TOKEN = os.environ["GH_TOKEN"]
OWNER, REPO = "Lukeskywalker704", "Better-Desktop"
API = f"https://api.github.com/repos/{OWNER}/{REPO}"
HEADERS = {
    "Authorization": f"token {TOKEN}",
    "Accept": "application/vnd.github+json",
    "User-Agent": "better-desktop-release",
}
TAG = sys.argv[1] if len(sys.argv) > 1 else "v0.1.3"
BODY_FILE = sys.argv[2] if len(sys.argv) > 2 else "release-v013.md"


def call(method, url, payload=None, raw=None, content_type="application/json"):
    data = json.dumps(payload).encode() if payload is not None else raw
    req = urllib.request.Request(url, data=data, method=method, headers=dict(HEADERS))
    if data is not None:
        req.add_header("Content-Type", content_type)
    try:
        with urllib.request.urlopen(req, timeout=90) as resp:
            body = resp.read()
    except urllib.error.HTTPError as exc:
        raise SystemExit(f"{method} {url} -> {exc.code}: {exc.read()[:300]}")
    return json.loads(body) if body else None


def git(*args):
    return subprocess.run(["git", "-c", "core.quotepath=false", *args],
                          capture_output=True, check=True).stdout.decode("utf-8", "replace").strip()


base = call("GET", f"{API}/git/ref/heads/main")["object"]["sha"]
base_tree = call("GET", f"{API}/git/commits/{base}")["tree"]["sha"]
changed = [f for f in git("diff", "--name-only", base, "HEAD").splitlines() if f]
print("base:", base, "changed:", len(changed))

entries = []
for path in changed:
    with open(path, "rb") as fh:
        blob = call("POST", f"{API}/git/blobs",
                    {"content": base64.b64encode(fh.read()).decode(), "encoding": "base64"})
    entries.append({"path": path.replace("\\", "/"), "mode": "100644", "type": "blob", "sha": blob["sha"]})

tree = call("POST", f"{API}/git/trees", {"base_tree": base_tree, "tree": entries})
commit = call("POST", f"{API}/git/commits",
              {"message": git("log", "-1", "--pretty=%B"), "tree": tree["sha"], "parents": [base]})
call("PATCH", f"{API}/git/refs/heads/main", {"sha": commit["sha"], "force": False})
print("commit:", commit["sha"])

try:
    call("POST", f"{API}/git/refs", {"ref": f"refs/tags/{TAG}", "sha": commit["sha"]})
    print("tag:", TAG)
except SystemExit as exc:
    print("tag exists:", exc)

body = open(BODY_FILE, encoding="utf-8").read().strip()
rel = call("POST", f"{API}/releases", {
    "tag_name": TAG, "target_commitish": "main", "name": TAG,
    "body": body, "draft": False, "prerelease": False,
})
print("release:", rel["html_url"])

with open("target/release/better-desktop.exe", "rb") as fh:
    exe = fh.read()
asset = call("POST", f"https://uploads.github.com/repos/{OWNER}/{REPO}/releases/{rel['id']}/assets?name=better-desktop.exe",
             raw=exe, content_type="application/octet-stream")
print(f"asset: {asset['name']} {asset['size'] / 1024 / 1024:.2f} MB")
print("REMOTE_SHA", commit["sha"])
