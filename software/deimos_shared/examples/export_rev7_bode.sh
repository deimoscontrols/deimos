#!/usr/bin/env bash

theme="${1:-light}"
output_dir="${2:-target/rev7_bode_export}"

find_on_path() {
    local name="$1"
    command -v "$name" 2>/dev/null || true
}

find_chromium() {
    if [[ -n "${BROWSER_PATH:-}" && -x "${BROWSER_PATH}" ]]; then
        printf '%s\n' "${BROWSER_PATH}"
        return 0
    fi

    local name
    for name in chromium chromium-browser google-chrome google-chrome-stable chrome; do
        local path
        path="$(find_on_path "$name")"
        if [[ -n "$path" ]]; then
            printf '%s\n' "$path"
            return 0
        fi
    done

    local path
    for path in \
        /usr/bin/chromium \
        /usr/bin/chromium-browser \
        /usr/bin/google-chrome \
        /usr/bin/google-chrome-stable \
        /usr/local/bin/chromium \
        /usr/local/bin/chromium-browser \
        /usr/local/bin/google-chrome \
        /snap/chromium/current/usr/lib/chromium-browser/chrome \
        /snap/bin/chromium \
        /var/lib/flatpak/app/org.chromium.Chromium/current/active/files/chromium/chrome \
        "${HOME}/.local/share/flatpak/app/org.chromium.Chromium/current/active/files/chromium/chrome"; do
        if [[ -x "$path" ]]; then
            printf '%s\n' "$path"
            return 0
        fi
    done

    return 1
}

if [[ -z "${BROWSER_PATH:-}" ]]; then
    if ! BROWSER_PATH="$(find_chromium)"; then
        printf 'error: could not find Chrome/Chromium; set BROWSER_PATH explicitly\n' >&2
        exit 1
    fi
    export BROWSER_PATH
fi

if [[ -z "${WEBDRIVER_PATH:-}" ]]; then
    chromedriver_path="$(find_on_path chromedriver)"
    if [[ -n "$chromedriver_path" ]]; then
        export WEBDRIVER_PATH="$chromedriver_path"
    elif [[ -x "${HOME}/.local/bin/chromedriver" ]]; then
        export WEBDRIVER_PATH="${HOME}/.local/bin/chromedriver"
    fi
fi

printf 'using BROWSER_PATH=%s\n' "$BROWSER_PATH"
if [[ -n "${WEBDRIVER_PATH:-}" ]]; then
    printf 'using WEBDRIVER_PATH=%s\n' "$WEBDRIVER_PATH"
fi

cargo run -p deimos_shared --features alloc --example rev7_bode -- "$theme" "$output_dir"
