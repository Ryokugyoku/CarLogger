#!/usr/bin/env bash
set -euo pipefail
tag="${1:?tag}"
previous=$(git tag --list 'v[0-9]*' --sort=-v:refname | head -1)
range="${previous:+$previous..}HEAD"
echo '## 新機能'; git log $range --pretty='- %s' --grep='^feat' || true
echo; echo '## 改善'; git log $range --pretty='- %s' --grep='^perf\|^refactor\|^improve' || true
echo; echo '## 不具合修正'; git log $range --pretty='- %s' --grep='^fix' || true
echo; echo '## その他の変更'
git log $range --pretty='- %s' --invert-grep --grep='^feat\|^perf\|^refactor\|^improve\|^fix' || true
echo; echo "対象バージョン: $tag"
