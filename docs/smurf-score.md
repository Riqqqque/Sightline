# The smurf flag

A hint that a player may be on a second account. It is a guess from public numbers, shown as
"Possible smurf", never as a fact. It can be turned off in Settings. The code is
`smurf_signals` in `src/logic.rs`.

## Design

The flag is meant to stay quiet. Playing well is not a signal; an experienced account on a good
run must never be flagged. So two things have to be true at once:

1. **The account looks new**, and
2. **its recent results are far above ordinary.**

## Signals and points

Account age (at least 1 point needed):

| Signal | Points |
| --- | --- |
| Account level under 30 | 2 |
| Account level 30 to 59 | 1 |
| Fewer than 40 ranked matches ever | 1 |

Recent form over the last ten ranked matches (at least 2 points needed; ignored with fewer than
five matches):

| Signal | Points |
| --- | --- |
| KD 1.60 or higher | 2 |
| KD 1.35 to 1.59 | 1 |
| Headshot rate 33% or higher | 2 |
| Headshot rate 28% to 32% | 1 |
| ADR 185 or higher | 1 |
| Won 70% or more of at least eight matches | 1 |

The flag shows when both groups meet their minimum and the total is 4 or more. Clicking the
player lists the signals that counted.

## What it cannot see

- A hidden player is never scored: there is nothing to score.
- A player who hides their account level can only be flagged through "fewer than 40 ranked
  matches ever".
- I am never scored.
- The thresholds are a first guess. They have not been checked against real lobbies yet; expect
  to tune them after a few sessions.
