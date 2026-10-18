#!/usr/bin/env bash
# Gitee 镜像同步脚本：在 Gitee Go 流水线（或 GitHub Actions 手动兜底）中运行。
# 从 GitHub 拉取 9 个仓库的 main 分支与最新 Release（含附件），
# 本地化镜像内的资源链接与显示文字后写入 Gitee。
# 空跑（无变化）时仅做 API 级检查，节省流水线构建时长额度。
set -u

TOKEN="${SYNC_GITEE_TOKEN:?environment variable SYNC_GITEE_TOKEN is required}"
OWNER="dqsq2e2"
REPOS="ting-reader ting-reader-flutter luci-app-tingreader ai-booklist-assistant ai-cover-generator ai-scraper-js rss-feed ting-reader-plugin-store ypshuo-scraper-wasm"
REPOS="${MIRROR_REPOS:-$REPOS}"
GH_API="https://api.github.com/repos"
GE_API="https://gitee.com/api/v5/repos"
PUSH_AUTH="$(printf '%s:%s' "$OWNER" "$TOKEN" | base64 -w0)"
LUCI="luci-app-tingreader"
ALT='ting-reader-plugin-store|ting-reader-flutter|ypshuo-scraper-wasm|ai-booklist-assistant|luci-app-tingreader|ai-cover-generator|ai-scraper-js|rss-feed|ting-reader'

command -v git >/dev/null 2>&1 || {
  apt-get update -qq >/dev/null 2>&1 && apt-get install -y -qq git curl >/dev/null 2>&1
}
command -v git >/dev/null 2>&1 || { echo "ERROR: git is required"; exit 1; }
command -v curl >/dev/null 2>&1 || { echo "ERROR: curl is required"; exit 1; }
command -v jq >/dev/null 2>&1 || {
  apt-get update -qq >/dev/null 2>&1 && apt-get install -y -qq jq >/dev/null 2>&1
}
command -v jq >/dev/null 2>&1 || { echo "ERROR: jq is required"; exit 1; }

github_get() {
  curl -sS --retry 3 --retry-delay 5 --connect-timeout 20 \
    -H 'Accept: application/vnd.github+json' "$1"
}

gitee_get() {
  curl -sS --retry 3 --retry-delay 2 --connect-timeout 20 "$1"
}

download() {
  curl -sSfL --retry 3 --retry-delay 5 --connect-timeout 20 \
    --speed-time 60 --speed-limit 1024 -o "$2" "$1"
}

# ---------- GitHub 侧目标 ----------

release_targets() {
  local repo="$1"
  if [ "$repo" = "$LUCI" ]; then
    github_get "$GH_API/$OWNER/$repo/releases?per_page=100" | jq -c '
      ( [.[] | select(.tag_name == "backend-latest")][0:1]
      + [.[] | select((.tag_name | startswith("backend-v")))][0:1]
      + [.[] | select((.tag_name | startswith("luci-v")))][0:1] )'
  else
    github_get "$GH_API/$OWNER/$repo/releases/latest" | jq -c '[.]'
  fi
}

github_main_head() {
  github_get "$GH_API/$OWNER/$1/branches/main" | jq -r '.commit.sha // empty'
}

# ---------- 链接与文字本地化 ----------

localize() {
  local work="$1" repo="$2" latest_tag="$3"
  find "$work" -type f -name '*.md' -not -path '*/.git/*' -print0 |
    xargs -0 -r perl -pi -e '
      s{https?://github\.com/dqsq2e2/('"$ALT"')(?![\w-])(?!/wiki)}{https://gitee.com/dqsq2e2/$1}g;
      s{https?://raw\.githubusercontent\.com/dqsq2e2/('"$ALT"')(?![\w-])/(.+?)(?=[)\s<>"\]]|$)}{https://gitee.com/dqsq2e2/$1/raw/$2}g;
      s{\[GitHub Releases\]}{[Gitee Releases]}g;
      s{!\[GitHub Release\]}{![Gitee Release]}g;
    '
  if [ -n "$latest_tag" ]; then
    find "$work" -type f -name '*.md' -not -path '*/.git/*' -print0 |
      xargs -0 -r env TAG="$latest_tag" perl -pi -e '
        s{https://gitee\.com/dqsq2e2/('"$ALT"')(?![\w-])/releases/latest/download/}{https://gitee.com/dqsq2e2/$1/releases/download/$ENV{TAG}/}g;
      '
  fi
  if [ "$repo" = "$LUCI" ]; then
    find "$work" -type f ! -path '*/.git/*' ! -path '*/.github/*' -print0 |
      xargs -0 -r perl -pi -e '
        s{https?://github\.com/dqsq2e2/('"$ALT"')(?![\w-])(?!/wiki)}{https://gitee.com/dqsq2e2/$1}g;
        s{(?<!https://)(?<!http://)github\.com/dqsq2e2/('"$ALT"')(?![\w-])(?!/wiki)}{gitee.com/dqsq2e2/$1}g;
        s{https?://raw\.githubusercontent\.com/dqsq2e2/('"$ALT"')(?![\w-])/(.+?)(?=[)\s<>"\]]|$)}{https://gitee.com/dqsq2e2/$1/raw/$2}g;
        s{gitee\.com/dqsq2e2/('"$ALT"')(?![\w-])/tags\b}{gitee.com/dqsq2e2/$1/releases}g;
        s{\bGitHub\b}{Gitee}g;
      '
  fi
}

# ---------- 同步逻辑 ----------

main_in_sync() {
  local repo="$1" gh_head="$2" latest_tag="$3" head msg
  head="$(gitee_get "$GE_API/$OWNER/$repo/commits?per_page=1" | jq -r '.[0].sha // empty')"
  [ -n "$head" ] || return 1
  msg="$(gitee_get "$GE_API/$OWNER/$repo/commits/$head" | jq -r '.commit.message // empty')"
  case "$msg" in
    *"[gh:$gh_head]"*"[tag:$latest_tag]"*) return 0 ;;
    *) return 1 ;;
  esac
}

sync_main() {
  local repo="$1" gh_head="$2" latest_tag="$3" work="/tmp/$repo"
  rm -rf "$work"
  git init -q "$work"
  git -C "$work" remote add origin "https://github.com/$OWNER/$repo.git"
  git -C "$work" fetch -q origin '+refs/heads/main:refs/remotes/origin/main' \
    || { echo "$repo: fetch main failed"; return 1; }
  git -C "$work" checkout -q -f -B main 'refs/remotes/origin/main'

  localize "$work" "$repo" "$latest_tag"

  if [ -n "$(git -C "$work" status --porcelain)" ]; then
    git -C "$work" add -A
    git -C "$work" -c user.name="$OWNER" \
      -c user.email='192683602+dqsq2e2@users.noreply.github.com' \
      commit -q -m "docs: localize links and texts for Gitee mirror [gh:$gh_head] [tag:$latest_tag]"
  elif ! git -C "$work" log -1 --format=%s | grep -q '\[gh:' 2>/dev/null; then
    # 无本地化差异但缺少同步标记（历史提交），补一个标记提交
    git -C "$work" -c user.name="$OWNER" \
      -c user.email='192683602+dqsq2e2@users.noreply.github.com' \
      commit -q --allow-empty \
      -m "docs: localize links and texts for Gitee mirror [gh:$gh_head] [tag:$latest_tag]"
  fi
  git -C "$work" -c "http.extraheader=Authorization: Basic $PUSH_AUTH" \
    push -q --force "https://gitee.com/$OWNER/$repo.git" 'refs/heads/main:refs/heads/main' \
    || { echo "$repo: push main failed"; return 1; }
}

release_in_sync() {
  local repo="$1" m="$2" tag rel id url fname
  tag="$(jq -r '.tag_name // empty' <<<"$m")"
  [ -n "$tag" ] || return 0
  rel="$(gitee_get "$GE_API/$OWNER/$repo/releases/tags/$tag")"
  id="$(jq -r '.id // empty' <<<"$rel")"
  [ -n "$id" ] || return 1
  while IFS=$'\t' read -r url fname; do
    [ -n "$fname" ] || continue
    jq -e --arg f "$fname" '.assets[]? | select(.name == $f)' <<<"$rel" >/dev/null || return 1
  done < <(jq -r '.assets[]? | [.browser_download_url, .name] | @tsv' <<<"$m")

  if [ "$tag" = "backend-latest" ]; then
    local gh_url gh_content ge_content
    gh_url="$(jq -r '.assets[]? | select(.name == "latest.json") | .browser_download_url' <<<"$m" | head -n 1)"
    [ -n "$gh_url" ] || return 0
    gh_content="$(curl -sSfL --retry 3 "$gh_url" || true)"
    ge_content="$(curl -sSfL --retry 2 "https://gitee.com/$OWNER/$repo/releases/download/$tag/latest.json" || true)"
    [ -n "$gh_content" ] && [ "$gh_content" = "$ge_content" ] || return 1
  fi
  return 0
}

sync_release() {
  local repo="$1" m="$2" work="$3" tag body rel id payload resp
  tag="$(jq -r '.tag_name // empty' <<<"$m")"
  body="$(jq -r '.body // empty' <<<"$m")"
  [ -n "$tag" ] || return 0
  echo "== $repo: sync release $tag"
  rel="$(gitee_get "$GE_API/$OWNER/$repo/releases/tags/$tag")"
  id="$(jq -r '.id // empty' <<<"$rel")"

  if [ "$tag" = "backend-latest" ] && [ -n "$id" ]; then
    local gh_url gh_content ge_content dcode
    gh_url="$(jq -r '.assets[]? | select(.name == "latest.json") | .browser_download_url' <<<"$m" | head -n 1)"
    gh_content=''; [ -z "$gh_url" ] || gh_content="$(curl -sSfL --retry 3 "$gh_url" || true)"
    ge_content="$(curl -sSfL --retry 2 "https://gitee.com/$OWNER/$repo/releases/download/$tag/latest.json" || true)"
    if [ -n "$gh_content" ] && [ "$gh_content" = "$ge_content" ]; then
      echo "$repo: backend-latest up to date"
      return 0
    fi
    echo "$repo: refresh backend-latest (latest.json changed)"
    dcode="$(curl -sS -o /dev/null -w '%{http_code}' \
      -X DELETE "$GE_API/$OWNER/$repo/releases/$id" -F "access_token=$TOKEN")"
    [ "$dcode" = "204" ] || { echo "$repo: delete backend-latest failed: HTTP $dcode"; return 1; }
    id=''
  fi

  if [ -z "$id" ]; then
    echo "== $repo: push tag $tag"
    git -C "$work" fetch -q origin "+refs/tags/$tag:refs/tags/$tag" \
      || { echo "$repo: fetch tag failed"; return 1; }
    git -C "$work" -c "http.extraheader=Authorization: Basic $PUSH_AUTH" \
      push -q --force "https://gitee.com/$OWNER/$repo.git" "refs/tags/$tag:refs/tags/$tag" \
      || { echo "$repo: push tag failed"; return 1; }
    payload="$(jq -n --arg tag "$tag" --arg body "$body" --arg owner "$OWNER" --arg repo "$repo" \
      '{ tag_name: $tag,
         name: $tag,
         body: (if ($body | length) > 0 then $body
                else "同步自 GitHub Release：https://github.com/\($owner)/\($repo)/releases/tag/\($tag)" end),
         target_commitish: "main" }')"
    resp="$(curl -sS -X POST "$GE_API/$OWNER/$repo/releases" \
      -F "access_token=$TOKEN" -F "tag_name=$tag" -F "name=$tag" \
      -F "body=$body" -F "target_commitish=main")"
    id="$(jq -r '.id // empty' <<<"$resp")"
    if [ -z "$id" ]; then
      echo "$repo: create release failed: $resp"
      return 1
    fi
    rel="$(gitee_get "$GE_API/$OWNER/$repo/releases/tags/$tag")"
  fi

  local assets_dir="/tmp/assets-$repo-${tag//\//-}" url fname up failed=0
  mkdir -p "$assets_dir"
  while IFS=$'\t' read -r url fname; do
    [ -n "$fname" ] || continue
    if jq -e --arg f "$fname" '.assets[]? | select(.name == $f)' <<<"$rel" >/dev/null; then
      continue
    fi
    echo "$repo: fetching asset $fname"
    download "$url" "$assets_dir/$fname" \
      || { echo "$repo: asset download failed: $fname"; failed=1; break; }
    up="$(curl -sS --connect-timeout 20 --speed-time 120 --speed-limit 10240 --max-time 2700 \
      -X POST "$GE_API/$OWNER/$repo/releases/$id/attach_files" \
      -F "access_token=$TOKEN" -F "file=@$assets_dir/$fname" -F "need_download_url=true")"
    if ! grep -q 'browser_download_url' <<<"$up"; then
      echo "$repo: asset upload failed: $fname: $up"
      failed=1
      break
    fi
    echo "$repo: uploaded $fname"
  done < <(jq -r '.assets[]? | [.browser_download_url, .name] | @tsv' <<<"$m")
  rm -rf "$assets_dir"
  [ "$failed" -eq 0 ] || return 1
  echo "$repo: release $tag up to date"
}

sync_repo() {
  local repo="$1" targets latest_tag gh_head i n m main_ok=0 rel_ok=0
  targets="$(release_targets "$repo")"
  n="$(jq 'length' <<<"$targets" 2>/dev/null || echo 0)"
  latest_tag="$(jq -r '.[0].tag_name // empty' <<<"$targets" 2>/dev/null)"
  gh_head="$(github_main_head "$repo")"
  echo "== $repo: targets: $(jq -r '.[].tag_name' <<<"$targets" 2>/dev/null | tr '\n' ' ')main-head: ${gh_head:-unknown}"

  # 空跑快速检查
  main_in_sync "$repo" "$gh_head" "$latest_tag" && main_ok=1
  rel_ok=1
  i=0
  while [ "$i" -lt "$n" ]; do
    release_in_sync "$repo" "$(jq -c ".[$i]" <<<"$targets")" || { rel_ok=0; break; }
    i=$((i + 1))
  done
  if [ "$main_ok" -eq 1 ] && [ "$rel_ok" -eq 1 ]; then
    echo "$repo: up to date (quick check)"
    return 0
  fi

  # 全量同步
  local work="/tmp/$repo"
  if [ "$main_ok" -eq 0 ]; then
    sync_main "$repo" "$gh_head" "$latest_tag" || return 1
  else
    # main 已同步，但 release 可能需要补附件：准备带 remote 的工作目录
    rm -rf "$work"
    git init -q "$work"
    git -C "$work" remote add origin "https://github.com/$OWNER/$repo.git"
  fi
  i=0
  while [ "$i" -lt "$n" ]; do
    m="$(jq -c ".[$i]" <<<"$targets")"
    if release_in_sync "$repo" "$m"; then
      :
    else
      sync_release "$repo" "$m" "$work" || return 1
    fi
    i=$((i + 1))
  done
  echo "$repo: up to date"
}

fail=0
for repo in $REPOS; do
  echo "---- $repo ----"
  if ! sync_repo "$repo"; then
    echo "WARNING: mirror of $repo failed, will retry next run"
    fail=1
  fi
  rm -rf "/tmp/$repo" "/tmp/assets-$repo-"*
done
exit $fail
