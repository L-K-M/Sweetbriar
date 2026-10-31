Final closed-window ID candidate. It adds the late-status-failure regression
and preserves NotFound for stopped reads; live reads still use the index gate.
Compare with the fresh r4c4-baseline at 2f961b3, using the same archive and
benchmark pacing. Report the exact built commit and machine state.

REF=963515c060e7d15fc0c0d634153c5686704ede5a
LABEL=r4c4-closed-id-final
RUNS=2
PROFILE=yes
SEARCH_TIMINGS=no
