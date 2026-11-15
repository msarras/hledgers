# hledgers vs hledger 1.99, linux-x86_64

Baseline: `/home/matt/.local/bin/hledger`. Median wall time of 5 runs (100 for household journals of up to 1000 transactions), the two binaries' runs alternating; peak RSS (VmHWM, polled every 2 ms); and whether stdout is identical. Written by `cargo run -p hledgers-bench -- --run`.

| journal | command | baseline | hledgers | speedup | RSS baseline | RSS hledgers | same stdout |
|---|---|---:|---:|---:|---:|---:|---|
| 0txns-household | balance | 31.1 ms | 6.3 ms | 5.0x | 22 MB | 8 MB | yes |
| 0txns-household | register | 30.1 ms | 6.0 ms | 5.0x | 21 MB | 7 MB | yes |
| 0txns-household | is | 31.6 ms | 6.6 ms | 4.8x | 22 MB | 8 MB | yes |
| 0txns-household | bs | 31.5 ms | 6.5 ms | 4.8x | 22 MB | 8 MB | yes |
| 0txns-household | roi --inv assets:investments --pnl income:investments -V | n/a | n/a | n/a | n/a | n/a | n/a |
| 10txns-household | balance | 37.1 ms | 8.0 ms | 4.7x | 24 MB | 8 MB | yes |
| 10txns-household | register | 41.9 ms | 7.8 ms | 5.4x | 28 MB | 8 MB | yes |
| 10txns-household | is | 37.6 ms | 8.0 ms | 4.7x | 24 MB | 9 MB | yes |
| 10txns-household | bs | 37.1 ms | 8.0 ms | 4.6x | 24 MB | 9 MB | yes |
| 10txns-household | roi --inv assets:investments --pnl income:investments -V | 39.6 ms | 8.9 ms | 4.4x | 24 MB | 9 MB | yes |
| 100txns-household | balance | 57.8 ms | 10.6 ms | 5.4x | 36 MB | 9 MB | yes |
| 100txns-household | register | 78.7 ms | 12.8 ms | 6.2x | 36 MB | 9 MB | yes |
| 100txns-household | is | 55.7 ms | 10.3 ms | 5.4x | 36 MB | 9 MB | yes |
| 100txns-household | bs | 55.4 ms | 10.3 ms | 5.4x | 36 MB | 9 MB | yes |
| 100txns-household | roi --inv assets:investments --pnl income:investments -V | 55.3 ms | 10.3 ms | 5.4x | 32 MB | 9 MB | yes |
| 1ktxns-household | balance | 168.1 ms | 31.7 ms | 5.3x | 40 MB | 11 MB | yes |
| 1ktxns-household | register | 515.2 ms | 67.5 ms | 7.6x | 48 MB | 15 MB | yes |
| 1ktxns-household | is | 167.3 ms | 31.6 ms | 5.3x | 40 MB | 12 MB | yes |
| 1ktxns-household | bs | 165.9 ms | 32.8 ms | 5.1x | 39 MB | 12 MB | yes |
| 1ktxns-household | roi --inv assets:investments --pnl income:investments -V | 204.7 ms | 32.5 ms | 6.3x | 36 MB | 11 MB | yes |
| 100ktxns-household | balance | 7.546 s | 1.468 s | 5.1x | 590 MB | 265 MB | yes |
| 100ktxns-household | register | 18.851 s | 3.030 s | 6.2x | 671 MB | 597 MB | yes |
| 100ktxns-household | is | 7.653 s | 1.445 s | 5.3x | 590 MB | 265 MB | yes |
| 100ktxns-household | bs | 8.306 s | 1.544 s | 5.4x | 609 MB | 265 MB | yes |
| 100ktxns-household | roi --inv assets:investments --pnl income:investments -V | 69.684 s | 1.373 s | 50.8x | 527 MB | 265 MB | yes |
| 10ktxns-1kaccts | stats | 806.0 ms | 70.0 ms | 11.5x | 76 MB | 39 MB | - |
| 10ktxns-1kaccts | balance | 1.282 s | 228.1 ms | 5.6x | 89 MB | 56 MB | yes |
| 10ktxns-1kaccts | print | 884.2 ms | 144.8 ms | 6.1x | 81 MB | 40 MB | yes |
| 10ktxns-1kaccts | register | 4.402 s | 832.6 ms | 5.3x | 106 MB | 189 MB | yes |
| 10ktxns-1kaccts | bal -M | 6.054 s | 1.833 s | 3.3x | 394 MB | 366 MB | yes |
| 10ktxns-1kaccts | reg -w 120 | 4.653 s | 934.0 ms | 5.0x | 107 MB | 217 MB | yes |
| 10ktxns-1kaccts | print -O json | 3.715 s | 1.123 s | 3.3x | 176 MB | 61 MB | yes |
| 100ktxns-1kaccts | stats | 6.749 s | 1.089 s | 6.2x | 563 MB | 294 MB | - |
| 100ktxns-1kaccts | balance | 7.128 s | 1.157 s | 6.2x | 567 MB | 295 MB | yes |
| 100ktxns-1kaccts | print | 9.319 s | 1.354 s | 6.9x | 565 MB | 295 MB | yes |
| 100ktxns-1kaccts | register | 36.872 s | 6.067 s | 6.1x | 641 MB | 1267 MB | yes |
| 100ktxns-1kaccts | bal -M | 113.831 s | 14.195 s | 8.0x | 3688 MB | 3618 MB | yes |
| 100ktxns-1kaccts | reg -w 120 | 41.461 s | 7.083 s | 5.9x | 641 MB | 1495 MB | yes |
| 100ktxns-1kaccts | print -O json | 36.156 s | 6.735 s | 5.4x | 1424 MB | 502 MB | yes |
| 10000x1000x10 (upstream) | stats | 639.8 ms | 100.3 ms | 6.4x | 66 MB | 32 MB | - |
| 10000x1000x10 (upstream) | balance | 937.8 ms | 111.9 ms | 8.4x | 76 MB | 33 MB | yes |
| 10000x1000x10 (upstream) | print | 1.264 s | 113.6 ms | 11.1x | 77 MB | 33 MB | yes |
| 10000x1000x10 (upstream) | register | 1.613 s | 196.3 ms | 8.2x | 77 MB | 43 MB | yes |
| 10000x1000x10 (upstream) | bal -M | 3.642 s | 923.8 ms | 3.9x | 368 MB | 172 MB | yes |
| 10000x1000x10 (upstream) | reg -w 120 | 1.731 s | 121.0 ms | 14.3x | 78 MB | 45 MB | yes |
| 10000x1000x10 (upstream) | print -O json | 3.370 s | 949.1 ms | 3.6x | 145 MB | 53 MB | yes |
