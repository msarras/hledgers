# hledgers vs hledger 1.99, linux-x86_64

Baseline: `/home/matt/.local/bin/hledger`. Median wall time of 5 runs, peak RSS (VmHWM, polled every 2 ms), and whether stdout is identical. Written by `cargo run -p hledgers-bench -- --run`.

| journal | command | baseline | hledgers | speedup | RSS baseline | RSS hledgers | same stdout |
|---|---|---:|---:|---:|---:|---:|---|
| 10ktxns-1kaccts | stats | 1.291 s | 0.154 s | 8.4x | 76 MB | 41 MB | - |
| 10ktxns-1kaccts | balance | 1.361 s | 0.314 s | 4.3x | 88 MB | 53 MB | yes |
| 10ktxns-1kaccts | print | 1.171 s | 0.172 s | 6.8x | 78 MB | 41 MB | yes |
| 10ktxns-1kaccts | register | 4.115 s | 1.013 s | 4.1x | 105 MB | 178 MB | yes |
| 10ktxns-1kaccts | bal -M | 7.421 s | 1.938 s | 3.8x | 393 MB | 362 MB | yes |
| 10ktxns-1kaccts | reg -w 120 | 4.945 s | 1.136 s | 4.4x | 106 MB | 213 MB | yes |
| 10ktxns-1kaccts | print -O json | 4.174 s | 2.038 s | 2.0x | 176 MB | 183 MB | yes |
| 100ktxns-1kaccts | stats | 7.273 s | 1.161 s | 6.3x | 563 MB | 294 MB | - |
| 100ktxns-1kaccts | balance | 6.546 s | 1.404 s | 4.7x | 567 MB | 293 MB | yes |
| 100ktxns-1kaccts | print | 10.386 s | 1.574 s | 6.6x | 566 MB | 293 MB | yes |
| 100ktxns-1kaccts | register | 39.057 s | 7.509 s | 5.2x | 642 MB | 1236 MB | yes |
| 100ktxns-1kaccts | bal -M | 117.588 s | 26.778 s | 4.4x | 3688 MB | 3560 MB | yes |
| 100ktxns-1kaccts | reg -w 120 | 45.833 s | 9.004 s | 5.1x | 641 MB | 1454 MB | yes |
| 100ktxns-1kaccts | print -O json | 37.626 s | 11.287 s | 3.3x | 1424 MB | 1698 MB | yes |
| 10000x1000x10 (upstream) | stats | 0.582 s | 0.129 s | 4.5x | 66 MB | 35 MB | - |
| 10000x1000x10 (upstream) | balance | 0.897 s | 0.088 s | 10.2x | 76 MB | 35 MB | yes |
| 10000x1000x10 (upstream) | print | 1.240 s | 0.108 s | 11.5x | 77 MB | 35 MB | yes |
| 10000x1000x10 (upstream) | register | 1.480 s | 0.285 s | 5.2x | 77 MB | 46 MB | yes |
| 10000x1000x10 (upstream) | bal -M | 3.692 s | 1.484 s | 2.5x | 368 MB | 167 MB | yes |
| 10000x1000x10 (upstream) | reg -w 120 | 1.462 s | 0.274 s | 5.3x | 77 MB | 47 MB | yes |
| 10000x1000x10 (upstream) | print -O json | 3.857 s | 1.175 s | 3.3x | 144 MB | 154 MB | yes |
