#!/usr/bin/env bash
# Offline regression checks: no real repositories or releases are modified.
set -euo pipefail
export SYNC_GITEE_TOKEN=fixture
if [[ "$(uname -s)" == MINGW* ]]; then
  jq() { command jq.exe -b "$@"; }
fi
source "$(dirname "${BASH_SOURCE[0]}")/mirror_sync.sh"

fail() { echo "FAIL: $*" >&2; exit 1; }
fixture='{"tag_name":"v1","assets":[]}'
head_fixture=0123456789abcdef0123456789abcdef01234567

(
  release_targets() { echo '{"message":"API rate limit"}'; }
  github_main_head() { fail "must reject malformed releases first"; }
  if sync_repo ting-reader; then fail "accepted an API error as a release"; fi
)
(
  release_targets() { echo "[$fixture]"; }
  github_main_head() { echo ''; }
  main_in_sync() { fail "must reject an empty source head"; }
  if sync_repo ting-reader; then fail "accepted a missing source head"; fi
)
(
  release_targets() { echo "[$fixture]"; }
  github_main_head() { echo "$head_fixture"; }
  main_in_sync() { return 0; }
  release_in_sync() { return 0; }
  sync_main() { fail "rewrote an up-to-date mirror"; }
  sync_release() { fail "rewrote an up-to-date release"; }
  sync_repo ting-reader
)
(
  release_targets() { echo "[$fixture]"; }
  github_main_head() { echo "$head_fixture"; }
  main_in_sync() { return 2; }
  sync_main() { fail "must not rewrite main after a Gitee API failure"; }
  if sync_repo ting-reader; then fail "ignored a Gitee API failure"; fi
)
(
  release_targets() { echo "[$fixture]"; }
  github_main_head() { echo "$head_fixture"; }
  main_in_sync() { return 0; }
  release_in_sync() { return 1; }
  sync_release() {
    [[ "$(git -C "$3" remote get-url origin)" == "https://github.com/dqsq2e2/ting-reader.git" ]] ||
      fail "missing checkout remote when repairing a release"
    touch "$RUN_DIR/repaired"
  }
  sync_repo ting-reader
  [[ -f "$RUN_DIR/repaired" ]] || fail "did not repair missing release assets"
)
(
  mkdir -p "$RUN_DIR/localize"
  echo '# Documentation' > "$RUN_DIR/localize/README.md"
  xargs() { return 1; }
  if localize "$RUN_DIR/localize" ting-reader v1; then fail "ignored localization failure"; fi
)
(
  gitee_get() { echo '{"id":1,"assets":[{"name":"latest.json"}]}'; }
  curl() {
    case "$*" in
      *attach_files*) echo '{"browser_download_url":"https://gitee.com/fixture"}' ;;
      *DELETE*) fail "deleted unchanged latest pointer" ;;
      *) echo '{"version":"2.0.6"}' ;;
    esac
  }
  download() { echo fixture > "$2"; touch "$RUN_DIR/downloaded"; }
  missing='{"tag_name":"backend-latest","assets":[{"name":"latest.json","browser_download_url":"https://github.com/latest"},{"name":"missing.json","browser_download_url":"https://github.com/missing"}]}'
  sync_release luci-app-tingreader "$missing" "$RUN_DIR/ting-reader"
  [[ -f "$RUN_DIR/downloaded" ]] || fail "unchanged pointer skipped a missing attachment"
)
(
  curl() { printf '{"message":"forbidden"}\n403'; }
  if gitee_get https://gitee.com/fixture; then fail "accepted denied API access"; fi
  curl() { printf '{"message":"not found"}\n404'; }
  [[ "$(gitee_get https://gitee.com/fixture)" == '{}' ]] || fail "missing release not recognized"
)
(
  gitee_get() { echo '{"id":1,"assets":[]}'; }
  download() { printf broken > "$2"; }
  curl() { fail "must not upload a truncated asset"; }
  truncated='{"tag_name":"v1","assets":[{"name":"package.tar.gz","browser_download_url":"https://github.com/fixture","size":100}]}'
  if sync_release ting-reader "$truncated" "$RUN_DIR/ting-reader"; then
    fail "accepted a truncated download"
  fi
)
(
  seed="$RUN_DIR/seed"
  snapshot="$RUN_DIR/upstream"
  mkdir -p "$seed" "$snapshot/root"
  command git init -q -b main "$seed"
  echo old > "$seed/README.md"
  echo obsolete > "$seed/obsolete.txt"
  command git -C "$seed" add -A
  command git -C "$seed" -c user.name=fixture -c user.email=fixture@example.invalid commit -q -m fixture
  echo updated > "$snapshot/root/README.md"
  echo ignored.txt > "$snapshot/root/.gitignore"
  echo tracked-upstream > "$snapshot/root/ignored.txt"
  tar -czf "$RUN_DIR/fixture.tar.gz" -C "$snapshot" root
  curl() {
    while [ "$#" -gt 0 ]; do
      if [ "$1" = -o ]; then cp "$RUN_DIR/fixture.tar.gz" "$2"; return; fi
      shift
    done
    fail "archive download did not specify a destination"
  }
  timeout() { shift 2; "$@"; }
  git() {
    local args=() arg
    for arg in "$@"; do
      if [ "$arg" = "https://gitee.com/dqsq2e2/ting-reader.git" ]; then
        args+=("$seed")
      else
        args+=("$arg")
      fi
    done
    command git "${args[@]}"
  }
  snapshot_from_github ting-reader "$head_fixture" "$RUN_DIR/ting-reader"
  [[ "$(cat "$RUN_DIR/ting-reader/README.md")" == updated ]] || fail "archive did not replace the mirror tree"
  [[ ! -e "$RUN_DIR/ting-reader/obsolete.txt" ]] || fail "archive retained a deleted upstream file"
  command git -C "$RUN_DIR/ting-reader" ls-files --error-unmatch ignored.txt >/dev/null ||
    fail "archive dropped an upstream tracked file matching .gitignore"
  if snapshot_from_github ting-reader "$head_fixture" "$RUN_DIR/seed"; then
    fail "archive replacement accepted a directory it does not own"
  fi
)
(
  expected="$(printf '{"version":"2.0.6"}\n' | sha256sum)"
  expected="${expected%% *}"
  gitee_get() { echo '{"id":1,"assets":[{"name":"latest.json"}]}'; }
  curl() {
    [[ "$*" != *github.com* ]] || fail "unchanged pointer caused an overseas download"
    while [ "$#" -gt 0 ]; do
      if [ "$1" = -o ]; then printf '{"version":"2.0.6"}\n' > "$2"; return; fi
      shift
    done
    fail "pointer download did not specify its destination"
  }
  pointer="$(jq -nc --arg hash "$expected" '{tag_name:"backend-latest",assets:[{name:"latest.json",digest:("sha256:"+$hash)}]}')"
  release_in_sync luci-app-tingreader "$pointer"
  curl() { fail "pointer was downloaded twice"; }
  release_in_sync luci-app-tingreader "$pointer"
)
echo 'All 11 mirror synchronization regression checks passed.'
