#!/usr/bin/env bash
# Generate <workspace>/.ld-java-analyze/classpath.txt for ld-java-analyze.
#
# Run from the root of the Maven project that VS Code opens as workspace.
# The current directory becomes the workspace root; every Maven output uses an
# absolute path, so multi-module reactors never scatter classpath.txt into
# submodules.
#
# Usage:
#   cd /path/to/project
#   /path/to/ld_java_analyze/scripts/setup-classpath.sh
#   /path/to/ld_java_analyze/scripts/setup-classpath.sh --module demo1-web
#   /path/to/ld_java_analyze/scripts/setup-classpath.sh --dry-run

set -euo pipefail

ROOT="$PWD"
STATE_DIR="$ROOT/.ld-java-analyze"
CLASSPATH_FILE="$STATE_DIR/classpath.txt"
MODULE=""
SKIP_INSTALL=0
SKIP_SOURCES=0
DRY_RUN=0

usage() {
  cat <<'EOF'
Usage: setup-classpath.sh [options]

  -m, --module NAME     Final module to use (default: probe all and keep widest)
      --skip-install    Skip `mvn -U install`
      --skip-sources    Skip `mvn dependency:sources`
  -n, --dry-run         Print commands without running Maven or writing files
  -h, --help            Show this help
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -m|--module)      MODULE="$2"; shift 2 ;;
    --skip-install)   SKIP_INSTALL=1; shift ;;
    --skip-sources)   SKIP_SOURCES=1; shift ;;
    -n|--dry-run)     DRY_RUN=1; shift ;;
    -h|--help)        usage; exit 0 ;;
    *) echo "Unknown option: $1" >&2; usage; exit 1 ;;
  esac
done

command -v mvn >/dev/null 2>&1 || { echo "mvn not found in PATH" >&2; exit 1; }
[[ -f "$ROOT/pom.xml" ]] || { echo "pom.xml not found in $ROOT - run from the project root" >&2; exit 1; }

echo "Project root : $ROOT"
echo "State dir    : $STATE_DIR"
echo "Output       : $CLASSPATH_FILE"
[[ $DRY_RUN -eq 1 ]] && echo "Mode         : dry run (no Maven calls, no files written)"

run_mvn() {
  local label="$1"; shift
  echo "==> $label"
  echo "    mvn $*"
  if [[ $DRY_RUN -eq 1 ]]; then
    return 0
  fi
  mvn "$@"
}

count_jars() {
  tr ';:' '\n' < "$1" \
    | sed 's/^[[:space:]]*//; s/[[:space:]]*$//; /^$/d; /^#/d' \
    | wc -l | tr -d ' '
}

declared_modules() {
  # Trim each line, keep line breaks: removing all whitespace would merge
  # every <module> name into a single token.
  sed -n 's/.*<module>\([^<]*\)<\/module>.*/\1/p' "$ROOT/pom.xml" \
    | sed 's/^[[:space:]]*//; s/[[:space:]]*$//' \
    | grep -v '^$' || true
}

[[ $DRY_RUN -eq 1 ]] || mkdir -p "$STATE_DIR"

if [[ $SKIP_INSTALL -eq 0 ]]; then
  # -U clears Maven's cached "resolution failed" markers.
  run_mvn 'Install reactor SNAPSHOT modules' -q -U install -DskipTests
fi
if [[ $SKIP_SOURCES -eq 0 ]]; then
  run_mvn 'Download sources jars' -q dependency:sources
fi

MODULES=()
while IFS= read -r line; do
  [[ -n "$line" ]] && MODULES+=("$line")
done < <(declared_modules)

if [[ -z "$MODULE" && ${#MODULES[@]} -eq 0 ]]; then
  run_mvn 'Build classpath (single module)' -q dependency:build-classpath \
    "-Dmdep.outputFile=$CLASSPATH_FILE"
elif [[ -n "$MODULE" ]]; then
  run_mvn "Build classpath for $MODULE" -q -pl "$MODULE" dependency:build-classpath \
    "-Dmdep.outputFile=$CLASSPATH_FILE"
else
  best_file=""
  best_count=-1
  for name in "${MODULES[@]}"; do
    safe="${name//\//_}"
    probe="$STATE_DIR/.probe-${safe}.txt"
    if ! run_mvn "Probe $name" -q -pl "$name" dependency:build-classpath \
      "-Dmdep.outputFile=$probe"; then
      echo "    warning: skipping $name" >&2
      rm -f "$probe"
      continue
    fi
    [[ $DRY_RUN -eq 1 ]] && continue
    count="$(count_jars "$probe")"
    echo "    $name : $count jars"
    if (( count > best_count )); then
      [[ -n "$best_file" ]] && rm -f "$best_file"
      best_file="$probe"
      best_count="$count"
    else
      rm -f "$probe"
    fi
  done
  if [[ $DRY_RUN -eq 0 ]]; then
    [[ -n "$best_file" ]] || { echo "No module produced a classpath" >&2; exit 1; }
    mv -f "$best_file" "$CLASSPATH_FILE"
    echo "==> Widest module classpath kept ($best_count jars)"
  fi
fi

if [[ $DRY_RUN -eq 1 ]]; then
  echo ''
  echo 'Dry run complete. Re-run without --dry-run to generate classpath.txt.'
  exit 0
fi

jars="$(count_jars "$CLASSPATH_FILE")"
existing=0
sources=0
while IFS= read -r jar; do
  [[ -n "$jar" ]] || continue
  [[ -f "$jar" ]] && existing=$((existing + 1))
  [[ -f "${jar%.jar}-sources.jar" ]] && sources=$((sources + 1))
done < <(tr ';:' '\n' < "$CLASSPATH_FILE" | grep -v -e '^$' -e '^#')

echo ''
echo "Done: $jars jars listed, $existing exist, $sources have sources jars"
echo "Written to $CLASSPATH_FILE"
echo "Next: in VS Code run 'ld-java-analyze: 重建索引', then 'ld-java-analyze: 查看索引状态'."
