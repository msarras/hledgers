# hledgers vs hledger 1.99, linux-x86_64

Baseline: `/home/matt/.local/bin/hledger`. Median wall time of 5 runs (100 for household journals of up to 1000 transactions), the two binaries' runs alternating; peak RSS (VmHWM, polled every 2 ms); and whether stdout is identical. Written by `cargo run -p hledgers-bench -- --run`.

| journal | command | baseline | hledgers | speedup | RSS baseline | RSS hledgers | same stdout |
|---|---|---:|---:|---:|---:|---:|---|
| 0txns-household | balance | 31.0 ms | 6.6 ms | 4.7x | 22 MB | 8 MB | yes |
| 0txns-household | register | 30.2 ms | 6.5 ms | 4.7x | 21 MB | 8 MB | yes |
| 0txns-household | is | 31.5 ms | 6.8 ms | 4.6x | 22 MB | 8 MB | yes |
| 0txns-household | bs | 31.3 ms | 6.8 ms | 4.6x | 22 MB | 8 MB | yes |
| 0txns-household | roi --inv assets:investments --pnl income:investments -V | n/a | n/a | n/a | n/a | n/a | n/a |
| 10txns-household | balance | 37.3 ms | 8.5 ms | 4.4x | 24 MB | 9 MB | yes |
| 10txns-household | register | 41.7 ms | 8.4 ms | 4.9x | 27 MB | 9 MB | yes |
| 10txns-household | is | 37.0 ms | 8.4 ms | 4.4x | 24 MB | 9 MB | yes |
| 10txns-household | bs | 36.7 ms | 8.5 ms | 4.3x | 24 MB | 9 MB | yes |
| 10txns-household | roi --inv assets:investments --pnl income:investments -V | 38.0 ms | 8.9 ms | 4.3x | 25 MB | 9 MB | yes |
| 100txns-household | balance | 56.2 ms | 11.4 ms | 4.9x | 36 MB | 9 MB | yes |
| 100txns-household | register | 78.8 ms | 14.9 ms | 5.3x | 36 MB | 10 MB | yes |
| 100txns-household | is | 56.1 ms | 11.5 ms | 4.9x | 36 MB | 10 MB | yes |
| 100txns-household | bs | 57.4 ms | 11.8 ms | 4.9x | 36 MB | 10 MB | yes |
| 100txns-household | roi --inv assets:investments --pnl income:investments -V | 55.8 ms | 11.7 ms | 4.8x | 32 MB | 9 MB | yes |
| 1ktxns-household | balance | 170.2 ms | 39.9 ms | 4.3x | 40 MB | 12 MB | yes |
| 1ktxns-household | register | 527.0 ms | 86.2 ms | 6.1x | 49 MB | 16 MB | yes |
| 1ktxns-household | is | 168.5 ms | 39.3 ms | 4.3x | 40 MB | 12 MB | yes |
| 1ktxns-household | bs | 168.5 ms | 41.2 ms | 4.1x | 40 MB | 12 MB | yes |
| 1ktxns-household | roi --inv assets:investments --pnl income:investments -V | 205.6 ms | 40.5 ms | 5.1x | 36 MB | 12 MB | yes |
| 100ktxns-household | balance | 7.599 s | 1.855 s | 4.1x | 590 MB | 296 MB | yes |
| 100ktxns-household | register | 21.559 s | 3.859 s | 5.6x | 671 MB | 609 MB | yes |
| 100ktxns-household | is | 7.842 s | 2.099 s | 3.7x | 590 MB | 297 MB | yes |
| 100ktxns-household | bs | 7.281 s | 2.201 s | 3.3x | 609 MB | 296 MB | yes |
| 100ktxns-household | roi --inv assets:investments --pnl income:investments -V | 71.338 s | 2.082 s | 34.3x | 527 MB | 298 MB | yes |
| 10ktxns-1kaccts | stats | 892.8 ms | 101.4 ms | 8.8x | 76 MB | 39 MB | - |
| 10ktxns-1kaccts | balance | 1.041 s | 129.4 ms | 8.0x | 88 MB | 52 MB | yes |
| 10ktxns-1kaccts | print | 1.468 s | 96.1 ms | 15.3x | 78 MB | 40 MB | yes |
| 10ktxns-1kaccts | register | 3.873 s | 937.6 ms | 4.1x | 106 MB | 177 MB | yes |
| 10ktxns-1kaccts | bal -M | 6.001 s | 2.235 s | 2.7x | 393 MB | 360 MB | yes |
| 10ktxns-1kaccts | reg -w 120 | 4.943 s | 1.095 s | 4.5x | 107 MB | 208 MB | yes |
| 10ktxns-1kaccts | print -O json | 3.894 s | 1.255 s | 3.1x | 176 MB | 182 MB | yes |
| 100ktxns-1kaccts | stats | 7.094 s | 1.069 s | 6.6x | 563 MB | 291 MB | - |
| 100ktxns-1kaccts | balance | 6.834 s | 1.161 s | 5.9x | 567 MB | 291 MB | yes |
| 100ktxns-1kaccts | print | 10.429 s | 1.453 s | 7.2x | 566 MB | 291 MB | yes |
| 100ktxns-1kaccts | register | 37.673 s | 6.980 s | 5.4x | 642 MB | 1231 MB | yes |
| 100ktxns-1kaccts | bal -M | 116.935 s | 24.401 s | 4.8x | 3687 MB | 3557 MB | yes |
| 100ktxns-1kaccts | reg -w 120 | 44.826 s | 8.193 s | 5.5x | 642 MB | 1452 MB | yes |
| 100ktxns-1kaccts | print -O json | 36.863 s | 10.280 s | 3.6x | 1424 MB | 1696 MB | yes |
| 10000x1000x10 (upstream) | stats | 803.0 ms | 118.3 ms | 6.8x | 66 MB | 33 MB | - |
| 10000x1000x10 (upstream) | balance | 711.4 ms | 143.8 ms | 4.9x | 76 MB | 33 MB | yes |
| 10000x1000x10 (upstream) | print | 1.050 s | 174.2 ms | 6.0x | 77 MB | 33 MB | yes |
| 10000x1000x10 (upstream) | register | 1.874 s | 154.3 ms | 12.1x | 78 MB | 45 MB | yes |
| 10000x1000x10 (upstream) | bal -M | 2.964 s | 1.069 s | 2.8x | 368 MB | 165 MB | yes |
| 10000x1000x10 (upstream) | reg -w 120 | 1.356 s | 168.7 ms | 8.0x | 78 MB | 44 MB | yes |
| 10000x1000x10 (upstream) | print -O json | 2.881 s | 1.184 s | 2.4x | 145 MB | 152 MB | yes |
