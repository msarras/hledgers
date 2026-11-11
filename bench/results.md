# hledgers vs hledger 1.99, linux-x86_64

Baseline: `/home/matt/.local/bin/hledger`. Median wall time of 5 runs (100 for household journals of up to 1000 transactions), the two binaries' runs alternating; peak RSS (VmHWM, polled every 2 ms); and whether stdout is identical. Written by `cargo run -p hledgers-bench -- --run`.

| journal | command | baseline | hledgers | speedup | RSS baseline | RSS hledgers | same stdout |
|---|---|---:|---:|---:|---:|---:|---|
| 0txns-household | balance | 31.2 ms | 6.3 ms | 5.0x | 22 MB | 8 MB | yes |
| 0txns-household | register | 30.7 ms | 6.0 ms | 5.1x | 21 MB | 7 MB | yes |
| 0txns-household | is | 31.7 ms | 6.5 ms | 4.9x | 22 MB | 8 MB | yes |
| 0txns-household | bs | 31.6 ms | 6.5 ms | 4.9x | 22 MB | 8 MB | yes |
| 0txns-household | roi --inv assets:investments --pnl income:investments -V | n/a | n/a | n/a | n/a | n/a | n/a |
| 10txns-household | balance | 37.4 ms | 7.8 ms | 4.8x | 24 MB | 8 MB | yes |
| 10txns-household | register | 41.9 ms | 7.8 ms | 5.4x | 28 MB | 8 MB | yes |
| 10txns-household | is | 37.8 ms | 8.0 ms | 4.7x | 24 MB | 9 MB | yes |
| 10txns-household | bs | 37.0 ms | 7.9 ms | 4.7x | 24 MB | 9 MB | yes |
| 10txns-household | roi --inv assets:investments --pnl income:investments -V | 38.2 ms | 8.3 ms | 4.6x | 25 MB | 9 MB | yes |
| 100txns-household | balance | 56.3 ms | 10.1 ms | 5.6x | 36 MB | 9 MB | yes |
| 100txns-household | register | 79.6 ms | 12.9 ms | 6.2x | 36 MB | 9 MB | yes |
| 100txns-household | is | 56.3 ms | 10.3 ms | 5.5x | 36 MB | 9 MB | yes |
| 100txns-household | bs | 56.5 ms | 10.3 ms | 5.5x | 36 MB | 9 MB | yes |
| 100txns-household | roi --inv assets:investments --pnl income:investments -V | 55.7 ms | 10.6 ms | 5.3x | 32 MB | 9 MB | yes |
| 1ktxns-household | balance | 170.0 ms | 31.7 ms | 5.4x | 40 MB | 11 MB | yes |
| 1ktxns-household | register | 524.5 ms | 67.9 ms | 7.7x | 48 MB | 15 MB | yes |
| 1ktxns-household | is | 168.3 ms | 31.5 ms | 5.3x | 40 MB | 12 MB | yes |
| 1ktxns-household | bs | 168.1 ms | 32.7 ms | 5.1x | 40 MB | 11 MB | yes |
| 1ktxns-household | roi --inv assets:investments --pnl income:investments -V | 207.4 ms | 32.3 ms | 6.4x | 36 MB | 11 MB | yes |
| 100ktxns-household | balance | 8.822 s | 1.527 s | 5.8x | 590 MB | 265 MB | yes |
| 100ktxns-household | register | 21.828 s | 3.241 s | 6.7x | 671 MB | 595 MB | yes |
| 100ktxns-household | is | 8.686 s | 1.480 s | 5.9x | 590 MB | 265 MB | yes |
| 100ktxns-household | bs | 8.526 s | 1.382 s | 6.2x | 609 MB | 265 MB | yes |
| 100ktxns-household | roi --inv assets:investments --pnl income:investments -V | 81.158 s | 1.530 s | 53.0x | 527 MB | 266 MB | yes |
| 10ktxns-1kaccts | stats | 1.029 s | 126.5 ms | 8.1x | 76 MB | 39 MB | - |
| 10ktxns-1kaccts | balance | 1.303 s | 239.7 ms | 5.4x | 88 MB | 56 MB | yes |
| 10ktxns-1kaccts | print | 1.287 s | 165.5 ms | 7.8x | 78 MB | 40 MB | yes |
| 10ktxns-1kaccts | register | 5.031 s | 863.1 ms | 5.8x | 106 MB | 193 MB | yes |
| 10ktxns-1kaccts | bal -M | 7.106 s | 1.749 s | 4.1x | 393 MB | 366 MB | yes |
| 10ktxns-1kaccts | reg -w 120 | 5.685 s | 1.047 s | 5.4x | 108 MB | 216 MB | yes |
| 10ktxns-1kaccts | print -O json | 4.075 s | 983.2 ms | 4.1x | 176 MB | 61 MB | yes |
| 100ktxns-1kaccts | stats | 6.759 s | 1.191 s | 5.7x | 563 MB | 294 MB | - |
| 100ktxns-1kaccts | balance | 7.260 s | 1.141 s | 6.4x | 567 MB | 295 MB | yes |
| 100ktxns-1kaccts | print | 12.394 s | 1.486 s | 8.3x | 565 MB | 294 MB | yes |
| 100ktxns-1kaccts | register | 43.097 s | 7.481 s | 5.8x | 641 MB | 1274 MB | yes |
| 100ktxns-1kaccts | bal -M | 125.908 s | 16.697 s | 7.5x | 3688 MB | 3619 MB | yes |
| 100ktxns-1kaccts | reg -w 120 | 48.336 s | 8.332 s | 5.8x | 643 MB | 1491 MB | yes |
| 100ktxns-1kaccts | print -O json | 40.566 s | 8.008 s | 5.1x | 1424 MB | 501 MB | yes |
| 10000x1000x10 (upstream) | stats | 802.0 ms | 110.1 ms | 7.3x | 66 MB | 32 MB | - |
| 10000x1000x10 (upstream) | balance | 740.9 ms | 90.8 ms | 8.2x | 76 MB | 32 MB | yes |
| 10000x1000x10 (upstream) | print | 1.555 s | 110.5 ms | 14.1x | 77 MB | 32 MB | yes |
| 10000x1000x10 (upstream) | register | 2.102 s | 163.6 ms | 12.8x | 78 MB | 44 MB | yes |
| 10000x1000x10 (upstream) | bal -M | 4.213 s | 1.061 s | 4.0x | 368 MB | 171 MB | yes |
| 10000x1000x10 (upstream) | reg -w 120 | 1.576 s | 179.4 ms | 8.8x | 78 MB | 45 MB | yes |
| 10000x1000x10 (upstream) | print -O json | 3.041 s | 998.2 ms | 3.0x | 145 MB | 53 MB | yes |
