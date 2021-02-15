#!/bin/sh

if [ -z "$1" ]
  then
    echo "[ERROR] Usage: $0 <dburl>"
    exit 1
fi

psql $1 -f `dirname "$0"`/schema.sql
