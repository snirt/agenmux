#!/usr/bin/env python3
"""Bake the latest GitHub release into site/index.html (in place, idempotent).

Usage: scripts/site-release.py [--release release.json] [site/index.html]
Without --release it fetches /releases/latest, using GITHUB_TOKEN or GH_TOKEN.
"""

import argparse
import html
import json
import os
import re
import sys
import urllib.request

REPO = "snirt/agenmux"
DEFAULT_PATH = os.path.join(os.path.dirname(__file__), "..", "site", "index.html")


def fetch_latest():
    req = urllib.request.Request(
        f"https://api.github.com/repos/{REPO}/releases/latest",
        headers={
            "Accept": "application/vnd.github+json",
            "User-Agent": "agenmux-site",
        },
    )
    token = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.load(resp)


def sub_once(pattern, repl, text, what):
    new, n = re.subn(pattern, repl, text, count=1, flags=re.S)
    if n != 1:
        sys.exit(f"site-release: could not find {what} in the page")
    return new


def render(page, rel):
    tag = rel["tag_name"]
    date = (rel.get("published_at") or "")[:10]
    body = (rel.get("body") or "").strip()

    page = sub_once(
        r'(<span id="version">)[^<]*(</span>)',
        lambda m: f"{m.group(1)}{html.escape(tag)}{m.group(2)}",
        page, "#version",
    )
    page = sub_once(
        r'(<span id="release-tag">)[^<]*(</span>)',
        lambda m: f"{m.group(1)}{html.escape(tag)}{m.group(2)}",
        page, "#release-tag",
    )
    page = sub_once(
        r'(<span id="release-date">)[^<]*(</span>)',
        lambda m: f"{m.group(1)}{html.escape(date)}{m.group(2)}",
        page, "#release-date",
    )
    hidden = "" if body else " hidden"
    page = sub_once(
        r'<pre id="release-notes"[^>]*><code id="release-body">.*?</code></pre>',
        lambda m: (
            f'<pre id="release-notes"{hidden}>'
            f'<code id="release-body">{html.escape(body)}</code></pre>'
        ),
        page, "#release-notes",
    )
    return page


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("path", nargs="?", default=DEFAULT_PATH)
    ap.add_argument("--release", help="JSON file with a release object")
    args = ap.parse_args()

    if args.release:
        with open(args.release, encoding="utf-8") as f:
            rel = json.load(f)
    else:
        rel = fetch_latest()

    with open(args.path, encoding="utf-8") as f:
        page = f.read()
    out = render(page, rel)
    if out != page:
        with open(args.path, "w", encoding="utf-8") as f:
            f.write(out)
    print(f"site-release: {args.path} -> {rel['tag_name']}")


if __name__ == "__main__":
    main()
