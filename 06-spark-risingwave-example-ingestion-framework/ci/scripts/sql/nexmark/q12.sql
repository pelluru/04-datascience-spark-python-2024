-- noinspection SqlNoDataSourceInspectionForFile
-- noinspection SqlResolveForFile
CREATE SINK nexmark_q12 AS
SELECT bidder, count(*) as bid_count, window_start, window_end
FROM TUMBLE(bid, p_time, INTERVAL '10' SECOND)
GROUP BY bidder, window_start, window_end
WITH ( connector = 'blackhole', type = 'append-only', force_append_only = 'true');
