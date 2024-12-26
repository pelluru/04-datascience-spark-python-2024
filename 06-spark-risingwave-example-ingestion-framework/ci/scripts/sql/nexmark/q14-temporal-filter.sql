-- noinspection SqlNoDataSourceInspectionForFile
-- noinspection SqlResolveForFile
CREATE SINK nexmark_q14_temporal_filter AS
SELECT auction,
       bidder,
       0.908 * price as price,
       CASE
           WHEN
                       extract(hour from date_time) >= 8 AND
                       extract(hour from date_time) <= 18
               THEN 'dayTime'
           WHEN
                       extract(hour from date_time) <= 6 OR
                       extract(hour from date_time) >= 20
               THEN 'nightTime'
           ELSE 'otherTime'
           END       AS bidTimeType,
       date_time
       -- extra
       -- TODO: count_char is an UDF, add it back when we support similar functionality.
       -- https://github.com/nexmark/nexmark/blob/master/nexmark-flink/src/main/java/com/github/nexmark/flink/udf/CountChar.java
       -- count_char(extra, 'c') AS c_counts
FROM bid_filtered
WHERE 0.908 * price > 1000000
  AND 0.908 * price < 50000000
WITH ( connector = 'blackhole', type = 'append-only', force_append_only = 'true');
