Stop admitted ID scans when their window closes, including sorts before the
first row. Compare with r4c4-baseline; keep archive and benchmark pacing fixed.
Record first-page latency, ID-scan worker time and per-query tails. The 100 ms
budget remains unchanged; this request does not assert it is met.

REF=9f21847ee3566910323e3df372d9bb226df27952
LABEL=r4c4-closed-id-reads
RUNS=2
PROFILE=yes
SEARCH_TIMINGS=no
