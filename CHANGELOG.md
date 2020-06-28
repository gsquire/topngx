## 0.3.1
- Pull Requests
  - https://github.com/gsquire/topngx/pull/8
- Make sure to clean the tail reading thread up.
- Remove the internal buffering count to get a quicker update while tailing.

## 0.3.0
- Pull Requests
  - https://github.com/gsquire/topngx/pull/7
- Change `--no-follow` to `--follow` allowing users to explicitly opt in for tailing log files.
- Change `-t` to `-i` for the interval argument.
- Bug Fixes
  - Only restore the cursor when running in tail mode.
  - Return an error if a user tries to tail standard input.

## 0.2.0
- Pull Requests
  - https://github.com/gsquire/topngx/pull/6
- Implement the first cut of log tailing.
