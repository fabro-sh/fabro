-- Diff per-test timings between two CSVs produced by `cargo dev bench-tests`.
--
-- Inputs: target/bench/old.csv and target/bench/new.csv (RFC 4180 CSVs with
-- columns: git_sha, run_index, started_at, binary, package, classname,
-- test_name, status, duration_ms). Both files may contain multiple SHAs;
-- this query expects exactly two distinct git_sha values across the union.
--
-- Output: one row per (package, test_name) where both SHAs have samples,
-- showing the change in median and p90 duration ordered by largest p50
-- regression. Skipped/failed rows are excluded so flakes don't pollute
-- timing data.
--
-- Run:
--   duckdb < test/analysis/bench-tests-diff.sql
--
-- Adjust the file paths in read_csv_auto(...) to point at your CSVs.

WITH samples AS (
    SELECT git_sha, package, test_name, duration_ms
    FROM read_csv_auto(['target/bench/old.csv', 'target/bench/new.csv'])
    WHERE status = 'passed'
),
stats AS (
    SELECT
        git_sha,
        package,
        test_name,
        count(*)                            AS n,
        median(duration_ms)                 AS p50_ms,
        quantile_cont(duration_ms, 0.9)     AS p90_ms,
        stddev_samp(duration_ms)            AS sd_ms
    FROM samples
    GROUP BY git_sha, package, test_name
),
paired AS (
    SELECT
        package,
        test_name,
        max_by(p50_ms, git_sha)  AS p50_new,
        min_by(p50_ms, git_sha)  AS p50_old,
        max_by(p90_ms, git_sha)  AS p90_new,
        min_by(p90_ms, git_sha)  AS p90_old,
        max_by(sd_ms,  git_sha)  AS sd_new,
        min_by(sd_ms,  git_sha)  AS sd_old,
        max_by(n,      git_sha)  AS n_new,
        min_by(n,      git_sha)  AS n_old
    FROM stats
    GROUP BY package, test_name
    HAVING count(DISTINCT git_sha) = 2
)
SELECT
    package,
    test_name,
    p50_old,
    p50_new,
    (p50_new - p50_old)                                              AS d_p50_ms,
    round(100.0 * (p50_new - p50_old) / nullif(p50_old, 0), 1)       AS d_p50_pct,
    p90_old,
    p90_new,
    (p90_new - p90_old)                                              AS d_p90_ms,
    sd_old,
    sd_new,
    n_old,
    n_new
FROM paired
ORDER BY d_p50_ms DESC NULLS LAST
LIMIT 50;

-- Aggregate view: same diff rolled up to the package level. Useful for
-- spotting whole-crate regressions before drilling into individual tests.
WITH samples AS (
    SELECT git_sha, package, duration_ms
    FROM read_csv_auto(['target/bench/old.csv', 'target/bench/new.csv'])
    WHERE status = 'passed'
),
package_stats AS (
    SELECT
        git_sha,
        package,
        count(*)                          AS n,
        sum(duration_ms)                  AS total_ms,
        median(duration_ms)               AS p50_ms,
        quantile_cont(duration_ms, 0.9)   AS p90_ms
    FROM samples
    GROUP BY git_sha, package
),
paired AS (
    SELECT
        package,
        max_by(total_ms, git_sha) AS total_new,
        min_by(total_ms, git_sha) AS total_old,
        max_by(p50_ms,   git_sha) AS p50_new,
        min_by(p50_ms,   git_sha) AS p50_old,
        max_by(p90_ms,   git_sha) AS p90_new,
        min_by(p90_ms,   git_sha) AS p90_old
    FROM package_stats
    GROUP BY package
    HAVING count(DISTINCT git_sha) = 2
)
SELECT
    package,
    total_old,
    total_new,
    (total_new - total_old)                                            AS d_total_ms,
    round(100.0 * (total_new - total_old) / nullif(total_old, 0), 1)   AS d_total_pct,
    p50_old,
    p50_new,
    p90_old,
    p90_new
FROM paired
ORDER BY d_total_ms DESC NULLS LAST;
