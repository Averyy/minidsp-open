# minidsp-open: Complete DSP Address Tables

All addresses extracted from MiniDSP Device Console v2.10.6 `DSP_default_parameters.xml` files.
Each address is a u16 index into DSP memory (4 bytes per index, float32 LE for new dialect devices).

Source: `MiniDSP Device Console.app/Contents/Resources/device_support_package/DSP_Structures/{dsp_id}/DSP_default_parameters.xml`

Cross-verified against minidsp-rs address tables for DSP 100 (2x4 HD) — all addresses match exactly.

## Address Table Status

| DSP ID | Device | Addresses extracted | In minidsp-rs | Status |
|--------|--------|-------------------|---------------|--------|
| 100 | 2x4 HD / Flex | Yes | Yes | Verified match |
| 101 | DDRC-24 / Flex DL | Yes | Yes | Verified match |
| 105 | SHD / SHD Studio / SHD Power | Yes | Yes (SHD only) | Add hw_id 21 + source map |
| **110** | **Flex Eight** | **Yes** | **No** | **NEW — ready to implement** |
| **111** | **Flex Eight DL** | **Yes** | **No** | **NEW — ready to implement** |
| **113** | **Flex HT/HTx old FW** | **Yes** | **Partial (HTx only)** | **NEW for Flex HT (hw_id 31)** |
| **115** | **Flex HT/HTx new FW** | **Yes** | **No** | **NEW — ready to implement** |

All 7 DSP structures are fully decoded. No hardware access needed. We can generate device definitions for every missing device right now.

## DSP 110 — Flex Eight (2 in / 8 out, 96kHz, FIR on input)

Params: addr=0, size=20392. No Dirac.

### Routing Matrix (2x8 with polarity)
```
Status:   Mixer_R_C_status  [0..15]   (2 rows × 8 cols)
Gain:     Mixer_R_C         [16..31]
Polarity: Mixer_R_C_pol     [32..47]
```

### Meters
```
Input:      Meter_In_1..2       [48..49]
Compressor: Meter_Comp_3..10    [50..57]
Output:     Meter_Out_3..10     [58..65]
```

### Input Channels (chn 1-2)

| Param | Input 1 | Input 2 |
|-------|---------|---------|
| Mute | 66 | 2170 |
| FIR bypass | 67 | 2171 |
| Gain | 68 | 2172 |
| Meter | 69 | 2173 |
| Polarity | 70 | 2174 |
| FIR taps count | 71 | 2175 |
| FIR data start | 72 | 2176 |
| FIR max taps | 2048 | 2048 |

Input PEQ (10 bands × 5 coeffs):

| Input | B1 | B2 | B3 | B4 | B5 | B6 | B7 | B8 | B9 | B10 |
|-------|------|------|------|------|------|------|------|------|------|------|
| 1 | 2120 | 2125 | 2130 | 2135 | 2140 | 2145 | 2150 | 2155 | 2160 | 2165 |
| 2 | 4224 | 4229 | 4234 | 4239 | 4244 | 4249 | 4254 | 4259 | 4264 | 4269 |

### Output Channels (chn 3-10, stride 103)

| Param | Out1 (ch3) | Out2 (ch4) | Out3 (ch5) | Out4 (ch6) | Out5 (ch7) | Out6 (ch8) | Out7 (ch9) | Out8 (ch10) |
|-------|-----------|-----------|-----------|-----------|-----------|-----------|-----------|------------|
| Mute | 4274 | 4377 | 4480 | 4583 | 4686 | 4789 | 4892 | 4995 |
| Comp bypass | 4275 | 4378 | 4481 | 4584 | 4687 | 4790 | 4893 | 4996 |
| Gain | 4276 | 4379 | 4482 | 4585 | 4688 | 4791 | 4894 | 4997 |
| Comp thresh | 4277 | 4380 | 4483 | 4586 | 4689 | 4792 | 4895 | 4998 |
| Comp gain | 4278 | 4381 | 4484 | 4587 | 4690 | 4793 | 4896 | 4999 |
| Comp ratio | 4279 | 4382 | 4485 | 4588 | 4691 | 4794 | 4897 | 5000 |
| Comp knee | 4280 | 4383 | 4486 | 4589 | 4692 | 4795 | 4898 | 5001 |
| Comp attack | 4281 | 4384 | 4487 | 4590 | 4693 | 4796 | 4899 | 5002 |
| Comp release | 4282 | 4385 | 4488 | 4591 | 4694 | 4797 | 4900 | 5003 |
| Delay | 4283 | 4386 | 4489 | 4592 | 4695 | 4798 | 4901 | 5004 |
| Polarity | 4286 | 4389 | 4492 | 4595 | 4698 | 4801 | 4904 | 5007 |
| PEQ B1 | 4287 | 4390 | 4493 | 4596 | 4699 | 4802 | 4905 | 5008 |
| PEQ B10 | 4332 | 4435 | 4538 | 4641 | 4744 | 4847 | 4950 | 5053 |
| Xover LPF | 4337 | 4440 | 4543 | 4646 | 4749 | 4852 | 4955 | 5058 |
| Xover HPF | 4357 | 4460 | 4563 | 4666 | 4769 | 4872 | 4975 | 5078 |

## DSP 111 — Flex Eight DL (2 in / 8 out, 48kHz, Dirac)

Params: addr=0, size=3568. Dirac: addr=3568, size=5124, 2 channels, 1088 FIR taps.

### Routing Matrix (2x8, no polarity)
```
Status: [0..15]   Gain: [16..31]
```

### Meters
```
Dirac In:   [32..33]    Dirac Out: [34..35]
Mixer:      [36..43]    Comp:      [44..51]    Output: [52..59]
```

### Output Channels (chn 1-8, stride 104)

| Param | Out1 | Out2 | Out3 | Out4 | Out5 | Out6 | Out7 | Out8 |
|-------|------|------|------|------|------|------|------|------|
| Mute | 60 | 164 | 268 | 372 | 476 | 580 | 684 | 788 |
| Comp bypass | 61 | 165 | 269 | 373 | 477 | 581 | 685 | 789 |
| Gain | 62 | 166 | 270 | 374 | 478 | 582 | 686 | 790 |
| Delay | 69 | 173 | 277 | 381 | 485 | 589 | 693 | 797 |
| Polarity | 73 | 177 | 281 | 385 | 489 | 593 | 697 | 801 |
| PEQ B1 | 74 | 178 | 282 | 386 | 490 | 594 | 698 | 802 |
| PEQ B10 | 119 | 223 | 327 | 431 | 535 | 639 | 743 | 847 |
| Xover LPF | 124 | 228 | 332 | 436 | 540 | 644 | 748 | 852 |
| Xover HPF | 144 | 248 | 352 | 456 | 560 | 664 | 768 | 872 |

## DSP 115 — Flex HT/HTx new FW (8 in / 8 out, 48kHz, bass mgmt + Dirac + input PEQ)

Params: addr=0, size=8224. Dirac: addr=8224, size=3332, 8 channels, 640 FIR taps.

### Bass Management BPF Crossover
```
Global LPF: [0..19]     Global HPF: [20..39]
BM gain status: [40..47]    BM gain: [48..55]
```

Per-channel BM BPF (8 channels):

| Ch | LPF | HPF |
|----|-----|-----|
| 1 | 272 | 292 |
| 2 | 312 | 332 |
| 3 | 352 | 372 |
| 4 | 392 | 412 |
| 5 | 432 | 452 |
| 6 | 472 | 492 |
| 7 | 512 | 532 |
| 8 | 552 | 572 |

### Bass Management Mixer (9×8 with polarity)
```
Status:   [56..127]    (9 rows × 8 cols)
Gain:     [128..199]
Polarity: [200..271]
```

### Output Routing Mixer (8×8 with polarity)
```
Status:   [592..655]
Gain:     [656..719]
Polarity: [720..783]
```

### Meters
```
Input:      [784..791]    Dirac In:  [792..799]
Dirac Out:  [800..807]    Comp:      [808..815]    Output: [816..823]
```

### Input PEQ (NEW in DSP 115, 8 channels × 10 bands × 5 coeffs)

| Ch | B1 | B2 | B3 | B4 | B5 | B6 | B7 | B8 | B9 | B10 |
|----|------|------|------|------|------|------|------|------|------|------|
| 1 | 1656 | 1661 | 1666 | 1671 | 1676 | 1681 | 1686 | 1691 | 1696 | 1701 |
| 2 | 1706 | 1711 | 1716 | 1721 | 1726 | 1731 | 1736 | 1741 | 1746 | 1751 |
| 3 | 1756 | 1761 | 1766 | 1771 | 1776 | 1781 | 1786 | 1791 | 1796 | 1801 |
| 4 | 1806 | 1811 | 1816 | 1821 | 1826 | 1831 | 1836 | 1841 | 1846 | 1851 |
| 5 | 1856 | 1861 | 1866 | 1871 | 1876 | 1881 | 1886 | 1891 | 1896 | 1901 |
| 6 | 1906 | 1911 | 1916 | 1921 | 1926 | 1931 | 1936 | 1941 | 1946 | 1951 |
| 7 | 1956 | 1961 | 1966 | 1971 | 1976 | 1981 | 1986 | 1991 | 1996 | 2001 |
| 8 | 2006 | 2011 | 2016 | 2021 | 2026 | 2031 | 2036 | 2041 | 2046 | 2051 |

### Output Channels (chn 9-16, stride 104)

| Param | Out1 (ch9) | Out2 (ch10) | Out3 (ch11) | Out4 (ch12) | Out5 (ch13) | Out6 (ch14) | Out7 (ch15) | Out8 (ch16) |
|-------|-----------|------------|------------|------------|------------|------------|------------|-------------|
| Mute | 824 | 928 | 1032 | 1136 | 1240 | 1344 | 1448 | 1552 |
| Comp bypass | 825 | 929 | 1033 | 1137 | 1241 | 1345 | 1449 | 1553 |
| Gain | 826 | 930 | 1034 | 1138 | 1242 | 1346 | 1450 | 1554 |
| Comp thresh | 827 | 931 | 1035 | 1139 | 1243 | 1347 | 1451 | 1555 |
| Delay | 833 | 937 | 1041 | 1145 | 1249 | 1353 | 1457 | 1561 |
| Polarity | 837 | 941 | 1045 | 1149 | 1253 | 1357 | 1461 | 1565 |
| PEQ B1 | 838 | 942 | 1046 | 1150 | 1254 | 1358 | 1462 | 1566 |
| PEQ B10 | 883 | 987 | 1091 | 1195 | 1299 | 1403 | 1507 | 1611 |
| Xover LPF | 888 | 992 | 1096 | 1200 | 1304 | 1408 | 1512 | 1616 |
| Xover HPF | 908 | 1012 | 1116 | 1220 | 1324 | 1428 | 1532 | 1636 |

## DSP 113 vs 115 Differences

DSP 113 (old firmware) is identical to 115 EXCEPT:
- No input PEQ (addresses 1656-2055 don't exist)
- Output channels use internal numbers 1-8 (not 9-16)
- Params size: 6624 (not 8224)
- Dirac base: 6624 (not 8224)
- Everything from addr 0 to 823 is identical between 113 and 115

## Notes

- All addresses verified against minidsp-rs for DSP 100 — exact match
- DSP 101 and 105 share the same parameter layout (addresses 0-1791), only Dirac block differs (FIR taps: 1088 vs 2176)
- DSP 113 and 115 share bass management + mixer layout (addresses 0-823), 115 adds input PEQ (1656-2055) and shifts output channel numbering
- Output channel stride: 103 (DSP 110), 104 (DSP 101/105/111/113/115)
- FIR data uses 2048 contiguous addresses per channel (DSP 100: outputs, DSP 110: inputs)
