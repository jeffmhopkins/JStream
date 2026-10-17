#!/bin/sh
# Writes seed inputs into corpus/<target>/ from files the repository already keeps, so the
# JSON targets start from real messages and collections instead of random bytes. Safe to
# re-run: seeds are named by content.
set -eu
cd "$(dirname "$0")"
root=..

seed() { # target, file
    mkdir -p "corpus/$1"
    cp "$2" "corpus/$1/seed-$(sha256sum "$2" | cut -c1-16)"
}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# Every client and server message in the protocol's example snapshot, one per seed.
grep '^{' "$root/crates/jstream-protocol/schema/examples.snap" | while IFS= read -r line; do
    printf '%s' "$line" > "$tmp/msg"
    seed protocol_decode "$tmp/msg"
done

# The checked-in collection file fixtures.
for f in "$root"/crates/jstream-graph/src/persist/fixtures/*.json; do
    seed collection_load "$f"
done

# Settings for each built-in kind: the first byte picks the kind.
i=0
while [ "$i" -lt 16 ]; do
    for settings in '{}' '{"color":"#ff00ff80","width":64,"height":64}' \
        '{"text":"Hello","size":48,"wrap_width":200,"outline_width":2}' \
        '{"font":{"type":"asset","root":"collection","path":"a.ttf"}}' \
        '{"device":"Microphone","exe":"game.exe"}'; do
        printf "\\$(printf '%03o' "$i")%s" "$settings" > "$tmp/settings"
        seed kind_settings "$tmp/settings"
    done
    i=$((i + 1))
done

# Image files the image source's tests decode, and the text golden PNGs. The repository
# keeps no GIF, so a 1x1 one is written here.
for f in "$root"/crates/jstream-sources/tests/data/*.jpg "$root"/crates/jstream-sources/tests/data/*.webp \
    "$root"/crates/jstream-sources/tests/data/text/*.png; do
    seed image_decode "$f"
done
printf 'GIF89a\001\000\001\000\200\000\000\377\000\000\000\000\000!\371\004\001\000\000\000\000,\000\000\000\000\001\000\001\000\000\002\002D\001\000;' > "$tmp/gif"
seed image_decode "$tmp/gif"

# The bundled font.
seed font_load "$root/crates/jstream-sources/assets/fonts/Roboto-Regular-subset.ttf"
