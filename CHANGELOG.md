## 0.3.0
- Change `--no-follow` to `--follow` allowing users to explicitly opt in for tailing log files.
- Bug Fixes
  - Only restore the cursor when running in tail mode.
  - Return an error if a user tries to tail standard input.

## 0.2.0
- Implement the first cut of log tailing.