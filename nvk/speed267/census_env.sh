#!/bin/bash
# census_env.sh <nakc> <ENV=VAL|-> : one tail line per kernel for that env
cd ~/nvmtl-build/speed267 || exit 3
for f in gbspv/spv/*.spv xlspv/*.spv; do
  if [ "$2" = "-" ]; then o=$(perl -e 'alarm 60; exec @ARGV' nakc/$1 $f main 2>&1 | tail -1)
  else o=$(env "$2" perl -e 'alarm 60; exec @ARGV' nakc/$1 $f main 2>&1 | tail -1); fi
  echo "$(basename $f)|$o"
done
