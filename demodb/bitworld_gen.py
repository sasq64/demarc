#!/usr/bin/env python3
"""Generate a release database from locally cached Janeway pages.

Iterates over pages/bitworld.<id>.html for the given id span and writes one
tab-separated line per valid release, each field prefixed with its name:

    id:<id> <tab> title:<title> <tab> author:<author> <tab> date:<date>
       <tab> party:<party> <tab> category:<category> <tab> tags:<tags>
       <tab> download:<download>

`tags` is a ';'-separated list. `download` is a ';'-separated list of all
"Direct Files" links from the downloads section (empty if the release has no
direct download).

By default all categories except Music are included; use --include to keep
only the listed categories, or --exclude to skip additional ones (both take
comma-separated, case-insensitive category names).

Usage:
    gendb.py START END [-o OUTPUT] [--include CATS | --exclude CATS]
"""

import argparse
import os
import re
import sys

# Header is "Title (Type) by Author"; some releases have no credited author.
META_RE = re.compile(r'(.*)\s+\(([^)]*)\)(?:\s+by\s+(.*))?$')
AKA_RE = re.compile(r'<span.*>aka .*</span>')
# Resolution/subtitle line inside the h1, e.g. <span class='subtitle'>112x484
SUBTITLE_RE = re.compile(r"<span class='subtitle'>.*?</span>", re.S)
ATTR_RE = re.compile(r'(\w+)=(?:["\']([^"\']*)["\'])')
TAG_RE = re.compile(r'<[^>]*>')
LINK_TEXT_RE = re.compile(r'<a\s[^>]*>([^<]*)</a>')
HREF_RE = re.compile(r"href='([^']+)'")
RELEASED_RE = re.compile(
    r"Released:\s*<em class='blacky'>(?:<a[^>]*>)?([^<]+)")
PARTY_RE = re.compile(r"<a href='party\.php\?id=\d+'>([^<]+)</a>")
CATEGORY_RE = re.compile(r"Categorized as:\s*<a[^>]*>([^<]+)</a>")
TAGS_RE = re.compile(r"<small class='tags'>(.*?)</small>", re.S)
DOWNLOADS_RE = re.compile(r"<div id='downloads'>")


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


def clean(text):
    """Collapse whitespace and strip tabs/newlines so fields stay one-line."""
    return re.sub(r'\s+', ' ', text).strip()


def parse_release(release_id, page):
    """Parse one release page; returns a name -> value dict or None."""
    contents, _ = find_tag(page, "div", {"class": "area symbolspace"})
    if not contents:
        return None

    header, _ = find_tag(contents, "h1")
    if header is None:
        return None
    header = AKA_RE.sub("", header)
    header = SUBTITLE_RE.sub("", header)
    m = META_RE.match(strip_tags(header))
    if not m:
        return None
    title = clean(m.group(1))
    author = clean(m.group(3)) if m.group(3) else ""

    m = RELEASED_RE.search(contents)
    date = clean(m.group(1)) if m else ""

    m = PARTY_RE.search(contents)
    party = clean(m.group(1)) if m else ""

    m = CATEGORY_RE.search(contents)
    category = clean(m.group(1)) if m else ""

    tags = []
    m = TAGS_RE.search(contents)
    if m:
        tags = [clean(t) for t in LINK_TEXT_RE.findall(m.group(1))]

    downloads_links = []
    downloads, _ = find_tag(page, "div", {"id": "downloads"})
    if downloads:
        # The release's own files live in the "Direct Files" group; other
        # groups hold featured modules etc.
        for group in downloads.split("<div class='group'>"):
            if group.lstrip().startswith("<h3>Direct Files</h3>"):
                # Each <li> bullet leads with the download link; any further
                # link on the line points to a related release, not a file.
                for item in group.split("<li>")[1:]:
                    hrefs = HREF_RE.findall(item)
                    if hrefs:
                        downloads_links.append(hrefs[0])
                break
    if not downloads_links:
        return None

    return {
        "id": str(release_id),
        "title": title,
        "author": author,
        "date": date,
        "party": party,
        "category": category,
        "tags": ";".join(tags),
        "download": ";".join(downloads_links),
    }


def main():
    parser = argparse.ArgumentParser(
        description="Generate release database from cached Janeway pages.")
    parser.add_argument("start", type=int, help="first release id (inclusive)")
    parser.add_argument("end", type=int, help="last release id (inclusive)")
    parser.add_argument("-o", "--output", default="releases.txt",
                        help="output file (default: releases.txt)")
    parser.add_argument("--pages", default="pages",
                        help="directory with cached pages (default: pages)")
    parser.add_argument("--include", metavar="CATS",
                        help="comma-separated categories to include "
                             "(default: all except Music)")
    parser.add_argument("--exclude", metavar="CATS", default="Music",
                        help="comma-separated categories to exclude "
                             "(default: Music); ignored if --include is given")
    args = parser.parse_args()

    include = None
    exclude = set()
    if args.include:
        include = {c.strip().lower() for c in args.include.split(",")}
    else:
        exclude = {c.strip().lower() for c in args.exclude.split(",")}

    written = 0
    missing = 0
    skipped = 0
    with open(args.output, "w") as out:
        for release_id in range(args.start, args.end + 1):
            file_name = os.path.join(args.pages, f"bitworld.{release_id}.html")
            if not os.path.exists(file_name):
                missing += 1
                continue
            with open(file_name, "rb") as fp:
                page = fp.read().decode("utf-8", "replace")
            fields = parse_release(release_id, page)
            if fields is None:
                skipped += 1
                continue
            category = fields["category"].lower()
            if (include is not None and category not in include) \
                    or category in exclude:
                skipped += 1
                continue
            out.write("\t".join(f"{name}:{value}"
                                for name, value in fields.items()) + "\n")
            written += 1

    print(f"{written} releases written to {args.output} "
          f"({skipped} pages skipped, {missing} ids missing)")


if __name__ == "__main__":
    sys.exit(main())
