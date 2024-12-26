#!/usr/bin/env bash

# NOTE(kwannoel): This script currently does not work locally...
# It has been adapted for CI use.

# USAGE: Script for generating queries via sqlsmith.
# These queries can be used for fuzz testing.
# Requires `$SNAPSHOT_DIR` to be set,
# that will be where queries are stored after generation.
#
# Example:
# SNAPSHOT_DIR="~/projects/sqlsmith-query-snapshots" ./gen_queries.sh

################# ENV

set -euo pipefail

export RUST_LOG="info"
export OUTDIR=$SNAPSHOT_DIR
export RW_HOME="../../../.."
export LOGDIR=".risingwave/log"
export TESTS_DIR="src/tests/sqlsmith/tests"
export TESTDATA="$TESTS_DIR/testdata"
export CRASH_MESSAGE="note: run with \`MADSIM_TEST_SEED=[0-9]*\` environment variable to reproduce this error"
export TIME_BOUND="6m"
export TEST_NUM_PER_SET=30
export E2E_TEST_NUM=64
export TIMEOUT_MESSAGE="Query Set timed out"

################## COMMON

refresh() {
  cd src/tests/sqlsmith/scripts
  source gen_queries.sh
  cd -
}

echo_err() {
  echo "$@" 1>&2
}

################## EXTRACT
# TODO(kwannoel): Write tests for these

# Get reason for generation crash.
# -m1 means that grep will early exit returning exit code 141, so we just ignore it for simplicity.
get_failure_reason() {
  set +e
  cat "$1" | tac | grep -B 10000 -m1 "\[EXECUTING" | tac | tail -n+2
  set -e
}

check_if_failed() {
  grep -B 2 "$CRASH_MESSAGE" || true
}

check_if_timeout() {
  grep "$TIMEOUT_MESSAGE" || true
}

# Extract queries from file $1, write to file $2
extract_queries() {
  local QUERIES=$(grep "\[EXECUTING .*\]: " < "$1" | sed -E 's/^.*\[EXECUTING .*\]: (.*)$/\1;/')
  local FAILED=$(check_if_failed < "$1")
  if [[ -n "$FAILED" ]]; then
    echo "Cluster crashed, see $1. Removing failed query."
    # Comment out the last line of queries.
    local QUERIES=$(echo -e "$QUERIES" | sed -E '$ s/(.*)/-- \1/')
  fi
  if [[ -n $(check_if_timeout < "$1") ]]; then
    echo "Cluster timed out, see $1. Removing last query in case."
    # Comment out the last line of queries.
    local QUERIES=$(echo -e "$QUERIES" | sed -E '$ s/(.*)/-- \1/')
  fi
  echo -e "$QUERIES" > "$2"
}

extract_ddl() {
  grep "\[EXECUTING CREATE .*\]: " | sed -E 's/^.*\[EXECUTING CREATE .*\]: (.*)$/\1;/' | $PG_FORMAT || true
}

extract_inserts() {
  grep "\[EXECUTING INSERT\]: " | sed -E 's/^.*\[EXECUTING INSERT\]: (.*)$/\1;/' || true
}

extract_updates() {
  grep "\[EXECUTING UPDATES\]: " | sed -E 's/^.*\[EXECUTING UPDATES\]: (.*)$/\1;/' || true
}

extract_last_session() {
  grep "\[EXECUTING TEST SESSION_VAR\]: " | sed -E 's/^.*\[EXECUTING TEST SESSION_VAR\]: (.*)$/\1;/' | tail -n 1 || true
}

extract_global_session() {
  grep "\[EXECUTING SET_VAR\]: " | sed -E 's/^.*\[EXECUTING SET_VAR\]: (.*)$/\1;/' || true
}

extract_failing_query() {
  grep "\[EXECUTING .*\]: " | tail -n 1 | sed -E 's/^.*\[EXECUTING .*\]: (.*)$/\1;/' | $PG_FORMAT || true
}

# FIXME(kwannoel): Extract from query-log instead.
# Extract fail info from [`generate-*.log`] in log dir
# $1 := log file name prefix. E.g. if file is generate-XXX.log, prefix will be "generate"
extract_fail_info_from_logs() {
  LOGFILE_PREFIX="$1"
  for LOGFILENAME in $(ls "$LOGDIR" | grep "$LOGFILE_PREFIX")
  do
    LOGFILE="$LOGDIR/$LOGFILENAME"
    echo_err "[INFO] Checking $LOGFILE for bugs"
    FAILED=$(check_if_failed < "$LOGFILE")
    echo_err "[INFO] Checked $LOGFILE for bugs"
    if [[ -n "$FAILED" ]]; then
      echo_err "[WARN] $LOGFILE Encountered bug."

      REASON=$(get_failure_reason "$LOGFILE")
      echo_err "[INFO] extracted fail reason for $LOGFILE"
      SEED=$(echo "$LOGFILENAME" | sed -E "s/${LOGFILE_PREFIX}\-(.*)\.log/\1/")

      DDL=$(extract_ddl < "$LOGFILE")
      echo_err "[INFO] extracted ddl for $LOGFILE"
      GLOBAL_SESSION=$(extract_global_session < "$LOGFILE")
      echo_err "[INFO] extracted global var for $LOGFILE"
      INSERTS=$(extract_inserts < "$LOGFILE")
      UPDATES=$(extract_updates < "$LOGFILE")
      echo_err "[INFO] extracted dml for $LOGFILE"
      TEST_SESSION=$(extract_last_session < "$LOGFILE")
      echo_err "[INFO] extracted session var for $LOGFILE"
      QUERY=$(extract_failing_query < "$LOGFILE")
      echo_err "[INFO] extracted failing query for $LOGFILE"

      FAIL_DIR="$OUTDIR/failed/$SEED"
      mkdir -p "$FAIL_DIR"

      echo -e "$DDL" \
       "\n\n$GLOBAL_SESSION" \
       "\n\n$INSERTS" \
       "\n\n$UPDATES" \
       "\n\n$TEST_SESSION" \
       "\n\n$QUERY" > "$FAIL_DIR/queries.sql"
      echo_err "[INFO] WROTE FAIL QUERY to $FAIL_DIR/queries.sql"
      echo -e "$REASON" > "$FAIL_DIR/fail.log"
      echo_err "[INFO] WROTE FAIL REASON to $FAIL_DIR/fail.log"

      cp "$LOGFILE" "$FAIL_DIR/$LOGFILENAME"
    fi
  done
}

################# Generate

# Generate $TEST_NUM number of seeds.
# if `ENABLE_RANDOM_SEED=1`, we will generate random seeds.
# sample output:
# 1 12329
# 2 22929
# 3 22921
gen_seed() {
  for i in $(seq 1 100)
  do
    if [[ $ENABLE_RANDOM_SEED -eq 1 ]]; then
      echo "$i $RANDOM"
    else
      echo "$i $i"
    fi
  done
}

# Prefer to use [`generate_deterministic`], it is faster since
# runs with all-in-one binary.
generate_deterministic() {
  set +e
  gen_seed | timeout 15m parallel --colsep ' ' "
    mkdir -p $OUTDIR/{1}
    echo '[INFO] Generating For Seed {2}, Query Set {1}'
    if MADSIM_TEST_SEED={2} timeout 5m $MADSIM_BIN \
      --sqlsmith $TEST_NUM_PER_SET \
      --generate-sqlsmith-queries $OUTDIR/{1} \
      $TESTDATA \
      2>$LOGDIR/generate-{1}.log;
    then
      echo '[INFO] Finished Generating For Seed {2}, Query set {1}'
    else
      echo '[INFO] Finished Generating For Seed {2}, Query set {1}'
      echo '[WARN] Cluster crashed or timed out while generating queries. see $LOGDIR/generate-{1}.log for more information.'
      if ! cat $LOGDIR/generate-{1}.log | grep '$CRASH_MESSAGE'
      then
        echo $TIMEOUT_MESSAGE >> $LOGDIR/generate-{1}.log
      fi
      buildkite-agent artifact upload "$LOGDIR/generate-{1}.log"
    fi
    "
  TIMED_OUT=$?
  set -e
  if [[ $TIMED_OUT -eq 124 ]]; then
    echo "TIMED_OUT"
    exit 124
  fi
  echo "--- Extracting queries"
  for i in $(seq 1 100);
  do
    echo "[INFO] Extracting Queries For Query set ${i}"
    extract_queries "${LOGDIR}/generate-${i}.log" "${OUTDIR}/${i}/queries.sql"
    echo "[INFO] Extracted Queries For Query set ${i}."
  done
  echo "Extracted all queries"
  wait
}

generate_sqlsmith() {
  mkdir -p "$OUTDIR/$1"
  ./risedev d
  ./target/debug/sqlsmith test \
    --testdata ./src/tests/sqlsmith/tests/testdata \
    --generate "$OUTDIR/$1"
}

############################# Checks

# Check that queries are different
check_different_queries() {
  if [[ -z $(diff "$OUTDIR/1/queries.sql" "$OUTDIR/2/queries.sql") ]]; then
    echo_err "[ERROR] Queries are the same! \
      Something went wrong in the generation process." \
      && exit 1
  fi
}

# Check that no queries are empty
check_queries_have_at_least_create_table() {
  for QUERY_FILE in "$OUTDIR"/*/queries.sql
  do
    set +e
    N_CREATE_TABLE="$(grep -c "CREATE TABLE" "$QUERY_FILE")"
    set -e
    if [[ $N_CREATE_TABLE -ge 1 ]]; then
      continue;
    else
      echo_err "[ERROR] Empty Query for $QUERY_FILE"
      cat "$QUERY_FILE"
      exit 1
    fi
  done
}

# Check if any query generation step failed, and any query file not generated.
check_failed_to_generate_queries() {
  if [[ "$(ls "$OUTDIR"/* | grep -c queries.sql)" -lt "$TEST_NUM" ]]; then
    echo_err "Queries not generated: "
    # FIXME(noel): This doesn't list the files which failed to be generated.
    ls "$OUTDIR"/* | grep queries.sql
    exit 1
  fi
}

# Run it to make sure it matches our expected timing for e2e test.
# Otherwise don't update this batch of queries yet.
run_queries_timed() {
  echo "" > $LOGDIR/run_deterministic.stdout.log
  timeout "$TIME_BOUND" seq $E2E_TEST_NUM | parallel "\
    timeout 6m $MADSIM_BIN --run-sqlsmith-queries $OUTDIR/{} \
      1>>$LOGDIR/run_deterministic.stdout.log \
      2>$LOGDIR/fuzzing-{}.log \
      && rm $LOGDIR/fuzzing-{}.log"
}

# Run it to make sure it should have no errors
run_queries() {
  set +e
  echo "" > $LOGDIR/run_deterministic.stdout.log
  seq $TEST_NUM | parallel "\
    timeout 15m $MADSIM_BIN --run-sqlsmith-queries $OUTDIR/{} \
      1>>$LOGDIR/run_deterministic.stdout.log \
      2>$LOGDIR/fuzzing-{}.log \
      && rm $LOGDIR/fuzzing-{}.log"
  set -e
}

# Generated query sets should not fail.
check_failed_to_run_queries() {
  FAILED_LOGS=$(ls "$LOGDIR" | grep fuzzing || true)
  if [[ -n "$FAILED_LOGS" ]]; then
    echo_err -e "FAILING_LOGS: $FAILED_LOGS"
    buildkite-agent artifact upload "$LOGDIR/fuzzing-*.log"
    exit 1
  fi
}

################### TOP LEVEL INTERFACE

setup() {
  echo "--- Installing pg_format"
  wget https://github.com/darold/pgFormatter/archive/refs/tags/v5.5.tar.gz
  tar -xvf v5.5.tar.gz
  export PG_FORMAT="$PWD/pgFormatter-5.5/pg_format"

  echo "--- Configuring Test variables"
  if [[ -z "$TEST_NUM" ]]; then
    echo "TEST_NUM unset, default to TEST_NUM=100"
    TEST_NUM=100
  fi
  if [[ -z "$ENABLE_RANDOM_SEED" ]]; then
    echo "ENABLE_RANDOM_SEED unset, default ENABLE_RANDOM_SEED=false (0)"
    ENABLE_RANDOM_SEED=0
  fi
  echo "[INFO]: TEST_NUM=$TEST_NUM"
  echo "[INFO]: ENABLE_RANDOM_SEED=$ENABLE_RANDOM_SEED"
  pushd $RW_HOME
  mkdir -p $LOGDIR
}

setup_madsim() {
  download-and-decompress-artifact risingwave_simulation .
  chmod +x ./risingwave_simulation
  export MADSIM_BIN="$PWD/risingwave_simulation"
}

build() {
  setup_madsim
  echo_err "[INFO] Finished setting up madsim"
}

generate() {
  generate_deterministic
}

validate() {
  check_different_queries
  echo_err "[CHECK PASSED] Generated queries should be different"
  check_failed_to_generate_queries
  echo_err "[CHECK PASSED] No seeds failed to generate queries"
  check_queries_have_at_least_create_table
  echo_err "[CHECK PASSED] All queries at least have CREATE TABLE"
  extract_fail_info_from_logs "generate"
  echo_err "[INFO] Recorded new bugs from  generated queries"
  echo "--- Running all queries check"
  run_queries
  echo "--- Check fail to run queries"
  check_failed_to_run_queries
  echo_err "[CHECK PASSED] Queries all ran without failure"
  echo_err "[INFO] Queries were ran and passed"
  echo "--- Running timeout check"
  run_queries_timed
  echo_err "[INFO] pre-generated queries running in e2e deterministic test are ran and passed in $TIME_BOUND"
  echo_err "[INFO] Passed checks"
}

# sync step
# Some queries maybe be added
sync_queries() {
  pushd $OUTDIR
  git stash
  git checkout main
  git pull
  set +e
  git branch -D old-main
  set -e
  git checkout -b old-main
  git push -f --set-upstream origin old-main
  git checkout -
  popd
}

sync() {
  echo_err "[INFO] Syncing"
  sync_queries
  echo_err "[INFO] Synced"
}

# Upload step
upload_queries() {
  git config --global user.email "buildkite-ci@risingwave-labs.com"
  git config --global user.name "Buildkite CI"
  set +x
  pushd "$OUTDIR"
  git add .
  git commit -m 'update queries'
  git push origin main
  popd
  set -x
}

upload() {
  echo_err "[INFO] Uploading Queries"
  upload_queries
  echo_err "[INFO] Uploaded"
}

cleanup() {
  popd
  echo_err "[INFO] Success!"
}

################### ENTRY POINTS

run_generate() {
  echo "--- Running setup"
  setup

  echo "--- Running build"
  build

  echo "--- Running synchronizing with upstream snapshot"
  sync

  echo "--- Generating"
  generate

  echo "--- Validating"
  validate

  echo "--- Uploading"
  upload

  echo "--- Cleanup"
  cleanup
}

run_extract() {
  LOGDIR="$PWD" OUTDIR="$PWD" extract_fail_info_from_logs "fuzzing"
  for QUERY_FOLDER in failed/*
  do
    QUERY_FILE="$QUERY_FOLDER/queries.sql"
    cargo build --bin sqlsmith-reducer
    REDUCER=$RW_HOME/target/debug/sqlsmith-reducer
    if [[ $($REDUCER --input-file "$QUERY_FILE" --output-file "$QUERY_FOLDER") -eq 0 ]]; then
      echo "[INFO] REDUCED QUERY: $PWD/$QUERY_FILE"
      echo "[INFO] WROTE TO DIR: $PWD/$QUERY_FOLDER"
    else
      echo "[INFO] FAILED TO REDUCE QUERY: $QUERY_FILE"
    fi
  done
}

main() {
  if [[ $1 == "extract" ]]; then
    echo "[INFO] Extracting queries"
    run_extract
  elif [[ $1 == "generate" ]]; then
    run_generate
  else
    echo "
================================================================
 Extract / Generate Sqlsmith queries
================================================================
 SYNOPSIS
    ./gen_queries.sh [COMMANDS]

 DESCRIPTION
    This script can extract sqlsmith queries from failing logs.
    It can also generate sqlsmith queries and store them in \$SNAPSHOT_DIR.

    You should be in \`risingwave/src/tests/sqlsmith/scripts\`
    when executing this script.

    (@kwannoel: Although eventually this should be integrated into risedev)

 COMMANDS
    generate                      Expects \$SNAPSHOT_DIR to be set.
    extract                       Extracts failing query from logs.
                                  E.g. fuzzing-66.log

 EXAMPLES
    # Generate queries
    SNAPSHOT_DIR=~/projects/sqlsmith-query-snapshots ./gen_queries.sh generate

    # Extract queries from log
    ./gen_queries.sh extract
"
  fi
}

main "$1"