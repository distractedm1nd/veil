#!/usr/bin/env bash
set -euo pipefail
veil_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
deps_root=$(dirname -- "$veil_root")

checkout() {
    local name=$1 url=$2 revision=$3
    local destination="$deps_root/$name"
    if [[ ! -d "$destination" ]]; then
        git clone --no-checkout "$url" "$destination"
        git -C "$destination" checkout --detach "$revision"
    fi
    if [[ $(git -C "$destination" rev-parse HEAD) != "$revision" ]]; then
        echo "$destination must be at $revision; existing checkout left untouched" >&2
        exit 1
    fi
}

patch_checkout() {
    local name=$1
    local patch="$veil_root/patches/$name.patch"
    if git -C "$deps_root/$name" apply --reverse --check "$patch" 2>/dev/null; then
        return
    fi
    git -C "$deps_root/$name" apply --check "$patch"
    git -C "$deps_root/$name" apply "$patch"
}

checkout wallet-libraries https://github.com/distractedm1nd/wallet-libraries.git b3314cba9e2200f8e647c850532f39a75db98784
checkout ztreamer https://github.com/distractedm1nd/ztreamer.git 541999709feceb1c9bfd2c0a49acc881fb031828
checkout zakura-veil https://github.com/zakura-core/zakura.git f4d44dd5cce281f0e35cbb84421c1f6a7474957c
patch_checkout zakura-veil
checkout ztreamer-veil https://github.com/distractedm1nd/ztreamer.git 541999709feceb1c9bfd2c0a49acc881fb031828
patch_checkout ztreamer-veil
