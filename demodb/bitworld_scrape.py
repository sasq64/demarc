#!/usr/bin/env python3
"""Scrape Amiga demo/intro releases from the Janeway (Exotica) database.

Port of ami.d. For each release id it fetches the release page, keeps only
demo/intro/trackmo/musicdisk productions that have a downloadable module, and
writes a tab-separated record per release to bitworld.txt:

    id <tab> title <tab> author <tab> type <tab> songs <tab> screenshots

`songs` is a ';'-separated list of module references, each prefixed with
"M:" (modland) or "A:" (amp.dascene). `screenshots` is a ';'-separated list of
screenshot paths relative to the kestra screenies directory. Every amp module
seen is additionally collected into amp.txt.

HTML pages are cached locally as bitworld.<id>.html so reruns don't refetch.
"""

import os
import re
import sys
from urllib.request import Request, urlopen

META_RE = re.compile(r'(.*)\s+\(([^)]*)\)\s+by\s+(.*)')
HREF_RE = re.compile(r"href='([^']+)'")
IMG_RE = re.compile(r'<img\s+[^>]*src=["\']([^"\']+)["\']')
AKA_RE = re.compile(r'<span.*>aka .*</span>')
ATTR_RE = re.compile(r'(\w+)=(?:["\']([^"\']*)["\'])')
TAG_RE = re.compile(r'<[^>]*>')


def get_tag_contents(src, tag):
    """Return the text inside the next <tag ...>HERE</tag>, honoring nesting."""
    start_tag = "<" + tag
    end_tag = "</" + tag + ">"

    start = src.lower().find(start_tag.lower())
    if start < 0:
        return None
    end = src.find('>', start)
    if end < 0:
        return None

    pos = end + 1
    depth = 0
    low = src.lower()
    start_low = start_tag.lower()
    end_low = end_tag.lower()
    while True:
        a = low.find(start_low, pos)
        b = low.find(end_low, pos)
        if b < 0:
            return None
        if a < 0 or b < a:
            # Found an end tag and no start tag before it.
            if depth > 0:
                depth -= 1
                pos = b + len(end_tag)
                continue
            return src[end + 1:b]
        # Found a nested start tag.
        depth += 1
        pos = a + len(start_tag)


def strip_tags(src):
    return TAG_RE.sub("", src)


def find_tag(src, tag, attributes=None):
    """Find the next <tag> whose attributes all match `attributes`.

    Returns a (contents, pos) tuple where `pos` is the offset of the opening
    tag, or (None, -1) if not found.
    """
    if attributes is None:
        attributes = {}
    start_tag = "<" + tag
    low = src.lower()
    start = 0
    while True:
        pos = low.find(start_tag.lower(), start)
        if pos < 0:
            return None, -1
        end = src.find('>', pos)
        if end < 0:
            return None, -1

        s = src[pos:end]
        match = 0
        for key, value in ATTR_RE.findall(s):
            if key in attributes and attributes[key] == value:
                match += 1
        if match == len(attributes):
            return get_tag_contents(src[pos:], tag), pos
        start = end + 1


def is_challenge(text):
    """True if `text` is the JS browser-verification stub, not real content."""
    return "Verifying your browser" in text


def fetch(file_name, url):
    """Return page text, fetching and caching it on first use.

    The site gates pages behind a JS challenge that sets a `verified=1`
    cookie and reloads. urlopen runs no JS, so we send the cookie (and a
    browser User-Agent) ourselves. The challenge stub is never cached, and a
    previously cached stub is treated as a miss so poisoned files self-heal.
    """
    if os.path.exists(file_name):
        with open(file_name, "rb") as fp:
            cached = fp.read().decode("utf-8", "replace")
        if not is_challenge(cached):
            return cached
    req = Request(url, headers={
        "User-Agent": "Mozilla/5.0",
        "Cookie": "verified=1",
    })
    data = urlopen(req).read()
    text = data.decode("utf-8", "replace")
    if is_challenge(text):
        raise RuntimeError(f"browser verification not bypassed for {url}")
    with open(file_name, "wb") as fp:
        fp.write(data)
    return text


def main():
    modland = "ftp://modland.ziphoid.com/pub/modules/"
    amp = "http://amp.dascene.net/modules/"
    screenies = "http://kestra.exotica.org.uk/files/screenies/"

    amp_coll = {}

    with open("bitworld.txt", "w") as out_file:
        for release_id in range(111667, 200000):
            file_name = f"bitworld.{release_id}.html"
            url = f"https://janeway.exotica.org.uk/release.php?id={release_id}"
            s = fetch(file_name, url)

            print("Check tag")
            contents, _ = find_tag(s, "div", {"class": "area symbolspace"})
            if not contents:
                continue
            print("Found contents")

            header, _ = find_tag(contents, "h1")
            if header is None:
                continue
            header = AKA_RE.sub("", header)
            line = strip_tags(header)
            c = META_RE.match(line)
            if not c:
                continue
            print("Found type")

            type_ = c.group(2)
            if type_.startswith("Crack"):
                continue
            if not (type_.endswith("emo") or type_.endswith("ntro")
                    or type_.endswith("ackmo") or type_ == "Musicdisk"):
                continue

            downloads, _ = find_tag(s, "div", {"id": "downloads"})
            print(f"Downloads {downloads}")
            songs = []
            if downloads:
                for url2 in HREF_RE.findall(downloads):
                    url2 = url2.replace("\n", ";")
                    if url2.startswith(modland):
                        songs.append("M:" + url2[len(modland):])
                    elif url2.startswith(amp):
                        songs.append("A:" + url2[len(amp):])
                        amp_coll[url2[len(amp):]] = 1

            if not songs:
                continue

            print(f"RELEASE {release_id}")

            shots, _ = find_tag(s, "div", {"class": "screenshots"})
            screenshots = []
            if shots:
                for src in IMG_RE.findall(shots):
                    if src.startswith(screenies):
                        screenshots.append(src[len(screenies):])

            out_file.write("\t".join([
                str(release_id),
                c.group(1),
                c.group(3),
                c.group(2),
                ";".join(songs),
                ";".join(screenshots),
            ]) + "\n")

    with open("amp.txt", "w") as out_file2:
        for a in amp_coll:
            out_file2.write(a + "\n")


if __name__ == "__main__":
    sys.exit(main())
