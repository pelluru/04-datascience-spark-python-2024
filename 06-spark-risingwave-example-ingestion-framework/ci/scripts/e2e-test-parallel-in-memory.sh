#!/usr/bin/env bash

# Exits as soon as any line fails.
set -euo pipefail

source ci/scripts/common.sh

while getopts 'p:' opt; do
    case ${opt} in
        p )
            profile=$OPTARG
            ;;
        \? )
            echo "Invalid Option: -$OPTARG" 1>&2
            exit 1
            ;;
        : )
            echo "Invalid option: $OPTARG requires an argument" 1>&2
            ;;
    esac
done
shift $((OPTIND -1))

download_and_prepare_rw "$profile" common

host_args=(-h localhost -p 4565 -h localhost -p 4566 -h localhost -p 4567)

echo "--- e2e, ci-3cn-3fe-in-memory, streaming"
risedev ci-start ci-3cn-3fe-in-memory
sqllogictest --version
sqllogictest "${host_args[@]}" -d dev './e2e_test/streaming/**/*.slt' --keep-db-on-failure -j 16 --junit "parallel-in-memory-streaming-${profile}" --label "in-memory" --label "parallel"

echo "--- Kill cluster"
risedev ci-kill

echo "--- e2e, ci-3cn-3fe-in-memory, batch"
risedev ci-start ci-3cn-3fe-in-memory
sqllogictest "${host_args[@]}" -d dev './e2e_test/ddl/**/*.slt' --junit "parallel-in-memory-batch-ddl-${profile}" --label "in-memory" --label "parallel"
sqllogictest "${host_args[@]}" -d dev './e2e_test/batch/**/*.slt' --keep-db-on-failure -j 16 --junit "parallel-in-memory-batch-${profile}" --label "in-memory" --label "parallel"

echo "--- Kill cluster"
risedev ci-kill