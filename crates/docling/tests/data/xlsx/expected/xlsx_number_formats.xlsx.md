| Item         |   Qty | Price       | Line total   |
|--------------|-------|-------------|--------------|
| Orange juice |    10 | $12.50      | $125.00      |
| Tea          |    20 | $7.25       | $145.00      |
|              |       | Subtotal    | $270.00      |
|              |       | Discount    | 10%          |
|              |       | Order month | Feb-25       |

| Format                | Value                      | Code                          |
|-----------------------|----------------------------|-------------------------------|
| Date                  | 2025-02-01                 | mm-dd-yy                      |
| Datetime 22           | 2025-02-01 13:30:00        | m/d/yy h:mm                   |
| Minutes               | 07:45                      | mm:ss                         |
| Afternoon             | 1:30 PM                    | h:mm AM/PM                    |
| Morning               | 12:07 AM                   | hh:mm AM/PM                   |
| 24h                   | 13:30                      | h:mm                          |
| Day month             | 1-Feb-25                   | d-mmm-yy                      |
| Due                   | Due Feb 1                  | "Due "mmm d                   |
| Due semicolon         | Due; Feb 1                 | "Due; "mmm d                  |
| ISO                   | 2025-02-01                 | yyyy-mm-dd                    |
| Weekday               | Saturday, February 1, 2025 | dddd, mmmm d, yyyy            |
| Locale currency       | $12.50                     | [$$-409]#,##0.00              |
| Negative parens       | ($5.00)                    | "$"#,##0.00_);\("$"#,##0.00\) |
| Negative red          | $-5.00                     | "$"#,##0.00;[Red]"$"-#,##0.00 |
| Negative plain        | -$5.00                     | "$"#,##0.00                   |
| Percent half up       | 13%                        | 0%                            |
| Percent neg           | -13%                       | 0%                            |
| Percent 2dp           | 1.25%                      | 0.00%                         |
| Currency half up      | $13                        | "$"#,##0                      |
| Currency neg half up  | -$13                       | "$"#,##0                      |
| Currency group        | $1,234.57                  | "$"#,##0.00                   |
| Zero dash             | -                          | "$"0;("$"0);"-"               |
| Zero empty            |                            | "$"0;("$"0);                  |
| Suffix                | 12.50$                     | 0.00"$"                       |
| Duration              | 1 day, 2:07:00             | [h]:mm:ss                     |
| Locale date raw       | 2025-02-01 00:00:00        | [$-409]mmmm d, yyyy           |
| Conditional raw       | 0.125                      | [>=1]0%;0.0%                  |
| Optional decimals raw | 12.5                       | "$"0.##                       |
| Digit literal raw     | 12.5                       | "$0"0.00                      |
| EUR raw               | 1234.5                     | "EUR" #,##0.00                |
| Thousands raw         | 1234567.891                | #,##0                         |
| Padded raw            | 42                         | 00000                         |
| Large                 | $123,456,789,012.35        | "$"#,##0.00                   |
| Tiny                  | 0.00%                      | 0.00%                         |
| Text format           | 3.5                        | @                             |
| Bool                  | True                       | 0%                            |
| Scientific raw        | 12345.678                  | 0.00E+00                      |
| Seconds               | 1:02:03 AM                 | h:mm:ss AM/PM                 |
| Timestamp 21          | 5:06:07                    | h:mm:ss                       |
| Date time text        | 2024-01-02 00:00:00        | yyyy\-mm\-dd\ hh:mm:ss        |
| Date time text h      | 2024-01-02 0:00:00         | yyyy\-mm\-dd\ h:mm:ss         |
| Escaped digits        | 1                          | \1\.0#                        |
