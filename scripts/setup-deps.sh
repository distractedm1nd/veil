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

checkout wallet-libraries https://github.com/distractedm1nd/wallet-libraries.git a9142ee100b3a563b7d9ba7a8e94201d00ad8154
checkout ztreamer https://github.com/distractedm1nd/ztreamer.git 1fdd51c037b7f5790556419d36e1d31ab3dcdf06
checkout zakura-veil https://github.com/zakura-core/zakura.git 8c35c23c1ac0834812937dd6d6aea3b8bb088600
checkout ztreamer-veil https://github.com/distractedm1nd/ztreamer.git 1fdd51c037b7f5790556419d36e1d31ab3dcdf06
