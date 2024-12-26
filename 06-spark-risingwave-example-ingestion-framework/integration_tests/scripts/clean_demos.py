#!/usr/bin/python3

from os.path import (dirname, abspath)
import os
import subprocess
import argparse
import sys


def clean_demo(demo: str):
    file_dir = dirname(abspath(__file__))
    project_dir = dirname(file_dir)
    demo_dir = os.path.join(project_dir, demo)
    print("Clean demo: {}".format(demo))

    subprocess.run(["docker", "compose", "down", "-v",
                   "--remove-orphans"], cwd=demo_dir, check=True)


arg_parser = argparse.ArgumentParser(description='Clean the demo')
arg_parser.add_argument('--case',
                        metavar='case',
                        type=str,
                        help='the test case')
args = arg_parser.parse_args()
demo = args.case

if demo in ['iceberg-sink', 'iceberg-source']:
    print('Skip for running test for `%s`' % demo)
    sys.exit(0)

clean_demo(demo)
